@db @serial(demo) @priority(-100)
Feature: cleaning up what this run created

  Every <<unique()>> token is "u" followed by <<run_id>>, the prefix drawn once per run,
  so one LIKE removes exactly the rows this run wrote and nothing another run owns. The
  "~" on the column name is what asks for LIKE; a bare "slug:" would compare exactly.

  The serial tag above puts this file in the same chain as db.feature, and the priority
  of -100 makes it the last file of that chain. Chains run in parallel, so this is "last
  here", not "last in the run" — a cleanup file and the files whose data it removes
  belong in one chain.

  Scenario: remove the companies this run created
    Given I have "companies" with "slug: <<unique()>>, name: Leftover Co"
    When I delete "companies" where "slug~: u<<run_id>>%"
    Then variable "deleted_companies" should not be equal to "0"
