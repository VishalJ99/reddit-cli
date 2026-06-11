# PER-280 license deferred

## Decision

Do not add an open-source license in the bootstrap commit.

## Rationale

The user asked for a public GitHub project, not an explicit reuse license. A public repository without a license is visible, but it does not grant broad reuse rights. Choosing MIT, Apache-2.0, GPL, or another license would materially change downstream rights and should be a human decision.

## Consequences

The README states that no license has been selected yet. Add a license in a later commit after the user chooses one.
