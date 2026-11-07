@db @serial(demo)
Feature: working with the database

  Demonstrates every DB step against the apibdd_demo schema (see examples/db/init.sql,
  spun up via docker-compose or manually with psql).

  Scenario: insert one row, read it back, extract a value
    Given I am in debug mode
    And I have "companies" with "slug: <<unique()>>, name: Acme Inc"
    Then I should have "companies" with "id: <<last_insert_id_companies>>"
    And I should have "companies" with "balance: 0"
    When I extract "balance" from "companies" with "id: <<last_insert_id_companies>>" as "acmeBalance"
    Then variable "acmeBalance" should be equal to "0"
    And I am not in debug mode

  Scenario: insert several rows via a table and verify with a table
    Given set variable "slug1" to "<<unique()>>"
    And I have "companies" where:
      | slug         | name   |
      | <<slug1>>    | First  |
      | <<unique()>> | Second |
    Then I should have "companies" with:
      | column | value     |
      | slug   | <<slug1>> |
      | name   | First     |

  Scenario: related tables via a shared "I have:" insert
    Given I have:
      | companies | slug: <<unique()>>, name: Wide Co                                        |
      | invoices  | company_id: <<last_insert_id_companies>>, number: INV-WIDE, amount: 42.50 |
    Then I should have "invoices" with "id: <<last_insert_id_invoices>>"
    And I should have "invoices" with "amount: 42.50"

  Scenario: explicit SQL NULL via <<null>>
    Given I have "companies" with "slug: <<unique()>>, deleted_at: <<null>>"
    Then I should have "companies" with "deleted_at: <<null>>"

  Scenario: update, delete one row, and full cleanup
    Given I have "companies" with "slug: <<unique()>>, name: Temp Co"
    When I update "companies" with "name: Renamed Co" where "id: <<last_insert_id_companies>>"
    Then variable "updated_companies" should be equal to "1"
    And I should have "companies" with "name: Renamed Co"
    When I delete "companies" where "id: <<last_insert_id_companies>>"
    Then variable "deleted_companies" should be equal to "1"
    And I should not have "companies" with "id: <<last_insert_id_companies>>"
    And I should not have "companies" with:
      | column | value                        |
      | id     | <<last_insert_id_companies>> |

    Given set variable "wipeCoSlug" to "<<unique()>>"
    And I have "companies" with "slug: <<wipeCoSlug>>"
    And I extract "id" from "companies" with "slug: <<wipeCoSlug>>" as "wipeCoId"
    And I have "invoices" with "company_id: <<wipeCoId>>, number: INV-WIPE-1, amount: 1"
    And I have "invoices" with "company_id: <<wipeCoId>>, number: INV-WIPE-2, amount: 2"
    When I delete all "invoices"
    Then variable "deleted_invoices" should not be equal to ""
    And I should not have "invoices" with "company_id: <<wipeCoId>>"

  Scenario: procedures, functions, and sequences
    Given I have "companies" with "slug: <<unique()>>, name: Routines Co, balance: 100"
    And I call procedure "apibdd_demo.recalc_balance" with "p_company_id: <<last_insert_id_companies>>, p_delta: 25"
    Then I should have "companies" with "balance: 125"

    When I call function "apibdd_demo.next_invoice_number" with "" as "invoiceNo"
    Then variable "invoiceNo" should not be equal to ""

    Given I get next value of sequence "apibdd_demo.invoice_seq" as "first"
    And I get next value of sequence "apibdd_demo.invoice_seq" as "second"
    Then variable "second" should not be equal to "<<first>>"

  Scenario: switching to another resource connection
    Given I use "reporting" connection
    And I have "audit_log" with "message: demo audit entry"
    Then I should have "audit_log" with "message: demo audit entry"

  Scenario: the four condition operators
    Given I have "companies" where:
      | slug                 | name       |
      | opdemo_a_<<run_id>>  | underscore |
      | opdemoXaY<<run_id>>  | neighbour  |
      | opdemo50%<<run_id>>  | percent    |
      | opdemokeep<<run_id>> | keeper     |

    # A bare "col:" is exact equality even when the value is nothing but wildcards.
    Then I should have "companies" with "slug: opdemo50%<<run_id>>"
    And I should not have "companies" with "slug: opdemo50Z<<run_id>>"

    # "col!:" is <> and "col!~:" is NOT LIKE.
    Then I should not have "companies" with "name!: keeper, slug: opdemokeep<<run_id>>"
    And I should have "companies" with "name!~: keep%, slug: opdemo_a_<<run_id>>"

    # Under a pattern a literal per cent is "\%": one row, not the whole opdemo50 family.
    When I delete "companies" where "slug~: opdemo50\%<<run_id>>"
    Then variable "deleted_companies" should be equal to "1"

    # Unmarked, that same "%" is data — this asks for a slug that literally contains it.
    When I delete "companies" where "slug: opdemokeep%<<run_id>>"
    Then variable "deleted_companies" should be equal to "0"

    # Under "~:" an "_" matches any single character, so the neighbour goes too.
    When I delete "companies" where "slug~: opdemo_a_<<run_id>>"
    Then variable "deleted_companies" should be equal to "2"

    # Whatever this scenario still owns. Its rows are prefixed, so no other file's are.
    When I delete "companies" where "slug~: opdemo%<<run_id>>"
    Then variable "deleted_companies" should be equal to "1"
