@db
Feature: what MariaDB adds over MySQL

  examples/db-features-mysql/db.feature is the engine-neutral file and passes on
  both engines. This one is the remainder: the two capabilities MariaDB has and
  MySQL does not, so it runs only from examples/db-mariadb.yaml. The extra schema
  is examples/db/init-mariadb.sql.

  Scenario: sequences
    Given I get next value of sequence "apibdd_demo.ticket_seq" as "first"
    And I get next value of sequence "apibdd_demo.ticket_seq" as "second"
    Then variable "second" should not be equal to "<<first>>"

    # The same step on MySQL fails with "sequences are not supported on mysql".

  Scenario: a primary key filled by a server-side DEFAULT
    # tickets.id defaults to NEXTVAL(ticket_seq) — server-generated, but not
    # AUTO_INCREMENT, so its value does not come back on the insert's own result
    # packet. Reading it needs RETURNING, which is what makes the variable below
    # exist at all: on MySQL this step is refused before the INSERT runs, naming
    # the column. That is the only way RETURNING is visible from a .feature file
    # — the clause itself only ever shows up in debug output.
    Given I am in debug mode
    And I have "tickets" with "subject: <<unique()>>"
    Then I should have "tickets" with "id: <<last_insert_id_tickets>>"
    And I am not in debug mode
