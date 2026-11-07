/// What `<<null>>` interpolates to. The host writes exactly these bytes into a
/// step's arguments and cells, and a NUL cannot occur in `.feature` text, so
/// the value cannot be typed by accident. It binds as SQL NULL and compiles to
/// `IS NULL` / `IS NOT NULL` in a WHERE.
pub const NULL_SENTINEL: &str = "\u{0}__bddkit_null__\u{0}";

/// A column value: `None` is SQL NULL, `Some` is text (for casting in SQL).
type Pair = (String, Option<String>);

fn make_value(raw: &str) -> Option<String> {
    let v = raw.trim();
    if v == NULL_SENTINEL {
        None
    } else {
        Some(v.to_string())
    }
}

/// Stands in for `\,` while a one-liner is split on commas.
const COMMA_ESCAPE: &str = "\u{0}comma\u{0}";

/// Parses `col:val, col2:val2`. An escaped comma `\,` is a literal.
/// A value equal to the NULL sentinel yields `None`. An empty string is an empty list.
/// An ambiguous piece without `:` is an explicit error, not a silent misparse.
pub fn parse_oneliner(s: &str) -> Result<Vec<Pair>, String> {
    if s.trim().is_empty() {
        return Ok(Vec::new());
    }
    let protected = s.replace("\\,", COMMA_ESCAPE);
    let mut out = Vec::new();
    for piece in protected.split(',') {
        let piece = piece.replace(COMMA_ESCAPE, ",");
        let (col, val) = piece
            .split_once(':')
            .ok_or_else(|| format!("failed to split column on ':' in {piece:?}"))?;
        let col = col.trim().to_string();
        if col.is_empty() {
            return Err(format!("empty column name in {piece:?}"));
        }
        out.push((col, make_value(val)));
    }
    Ok(out)
}

/// A procedure/function call argument. The type is guessed from the text because
/// we do not introspect the signature; this is enough for int/float/bool/text.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

/// Guesses the argument type from the text (`i64` → `f64` → `bool` → text).
pub fn infer_arg(raw: &str) -> Arg {
    let v = raw.trim();
    if v == NULL_SENTINEL {
        return Arg::Null;
    }
    if let Ok(i) = v.parse::<i64>() {
        return Arg::Int(i);
    }
    if let Ok(f) = v.parse::<f64>() {
        return Arg::Float(f);
    }
    match v {
        "true" => Arg::Bool(true),
        "false" => Arg::Bool(false),
        _ => Arg::Text(v.to_string()),
    }
}

/// Parses positional arguments `a:1,b:2` into a value order;
/// names are documentation only, casting follows the `infer_arg` heuristic.
pub fn parse_args(s: &str) -> Result<Vec<Arg>, String> {
    Ok(parse_oneliner(s)?
        .into_iter()
        .map(|(_, v)| match v {
            None => Arg::Null,
            Some(t) => infer_arg(&t),
        })
        .collect())
}

/// A "wide" table `I have "T" where:`: the first row is column names,
/// each following one is values. The result is one set of pairs per data row.
pub fn pairs_from_wide(rows: &[Vec<String>]) -> Result<Vec<Vec<Pair>>, String> {
    let header = rows.first().ok_or("table is empty")?;
    let mut result = Vec::new();
    for row in &rows[1..] {
        let mut pairs = Vec::with_capacity(header.len());
        for (i, col) in header.iter().enumerate() {
            let raw = row.get(i).map_or("", String::as_str);
            pairs.push((col.trim().to_string(), make_value(raw)));
        }
        result.push(pairs);
    }
    Ok(result)
}

/// A "tall" table `I should have "T" with:`: the first row is the header
/// (`| column | value |`), each following one is a (column, value) pair.
pub fn pairs_from_tall(rows: &[Vec<String>]) -> Result<Vec<Pair>, String> {
    let mut out = Vec::new();
    for row in rows.iter().skip(1) {
        let col = row.first().ok_or("table row is empty")?.trim().to_string();
        let raw = row.get(1).map_or("", String::as_str);
        out.push((col, make_value(raw)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host interpolates `<<null>>` to exactly these bytes; a drift would
    /// turn NULL into a literal string.
    #[test]
    fn the_null_sentinel_is_the_hosts_bytes() {
        assert_eq!(NULL_SENTINEL.as_bytes(), b"\0__bddkit_null__\0");
    }

    #[test]
    fn oneliner_splits_columns() {
        let p = parse_oneliner("name: Root, company_id: 5").unwrap();
        assert_eq!(
            p,
            vec![
                ("name".into(), Some("Root".into())),
                ("company_id".into(), Some("5".into())),
            ]
        );
    }

    #[test]
    fn oneliner_null_sentinel_becomes_none() {
        let raw = format!("deleted_at: {NULL_SENTINEL}");
        let p = parse_oneliner(&raw).unwrap();
        assert_eq!(p, vec![("deleted_at".into(), None)]);
    }

    #[test]
    fn oneliner_escaped_comma_is_literal() {
        let p = parse_oneliner(r"name: a\, b").unwrap();
        assert_eq!(p, vec![("name".into(), Some("a, b".into()))]);
    }

    #[test]
    fn oneliner_missing_colon_is_error() {
        assert!(parse_oneliner("just_a_value").is_err());
    }

    #[test]
    fn oneliner_empty_is_empty() {
        assert!(parse_oneliner("").unwrap().is_empty());
    }

    #[test]
    fn wide_table_one_set_per_data_row() {
        let rows = vec![
            vec!["company_id".to_string(), "name".to_string()],
            vec!["5".to_string(), "first".to_string()],
            vec!["6".to_string(), "second".to_string()],
        ];
        let sets = pairs_from_wide(&rows).unwrap();
        assert_eq!(sets.len(), 2);
        assert_eq!(
            sets[0],
            vec![
                ("company_id".into(), Some("5".into())),
                ("name".into(), Some("first".into())),
            ]
        );
        assert_eq!(sets[1][1], ("name".into(), Some("second".into())));
    }

    #[test]
    fn tall_table_is_column_value_pairs() {
        let rows = vec![
            vec!["column".to_string(), "value".to_string()],
            vec!["slug".to_string(), "abc".to_string()],
            vec!["id".to_string(), "42".to_string()],
        ];
        let p = pairs_from_tall(&rows).unwrap();
        assert_eq!(
            p,
            vec![
                ("slug".into(), Some("abc".into())),
                ("id".into(), Some("42".into())),
            ]
        );
    }

    #[test]
    fn infer_arg_guesses_type_by_text() {
        assert_eq!(infer_arg("42"), Arg::Int(42));
        assert_eq!(infer_arg("1.5"), Arg::Float(1.5));
        assert_eq!(infer_arg("true"), Arg::Bool(true));
        assert_eq!(infer_arg("false"), Arg::Bool(false));
        assert_eq!(infer_arg("abc"), Arg::Text("abc".into()));
        assert_eq!(infer_arg(NULL_SENTINEL), Arg::Null);
    }

    #[test]
    fn parse_args_positional_in_write_order() {
        let args = parse_args("a: 1, b: two, c: 3.0").unwrap();
        assert_eq!(
            args,
            vec![Arg::Int(1), Arg::Text("two".into()), Arg::Float(3.0)]
        );
        assert!(parse_args("").unwrap().is_empty());
    }
}
