# PER-283 thread request budget

## Context

`DESIGN.md` calls for `rdt thread --all` and `rdt pull` to expand hidden comment stubs while respecting `--max-requests`. The budget could count only expansion requests, or it could count the initial thread fetch plus all follow-up expansion requests.

## Decision

Count the initial thread fetch as one request in `--max-requests`.

## Consequences

`--max-requests 1` means "fetch the initial thread only and report any unresolved stubs." Higher values add more `morechildren` or continue-thread fetches up to the total cap. This is conservative and easy to explain: the configured cap is the maximum number of Reddit thread HTTP requests a command will make. If the cap is reached, `ThreadView.truncated` is true and the notice reports the remaining hidden comment count.
