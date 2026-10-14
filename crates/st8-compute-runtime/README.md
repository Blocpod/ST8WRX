# Private execution and recovery

`st8-compute run` requires `--private-output /private/path/answer.txt` in addition
to `--output /path/receipt.json`. Parent directories must already exist. The
answer is written exclusively (no overwrite), with mode 0600 on Unix, before
meter validation. Its raw bytes hash to the public receipt's output commitment.
Only commitments and signed accounting events enter the relay or receipt file.
The private answer may still be available if accounting subsequently fails.

`--max-output-tokens` defaults to 512 (maximum 32768). `--expires-in-secs` is
1–3600 seconds, default 600. The HTTP request uses the remaining job deadline;
private requests never follow redirects, and API/console endpoints must use
literal loopback IP addresses. Budget preflight checks the fixed charge, minimum
input/time usage, one job, and the requested output allowance. Actual input and
runtime costs are checked against the signed cap after execution; this does not
reserve funds or guarantee the requested work fits the cap.

The sibling `receipt.state.json` saves exact signed events and the publication
cursor before network writes, with file/directory sync. Retry with:

```sh
st8-compute resume --state /path/receipt.state.json
```

Use the same requester/provider keys as the original run. Resume republishes
only pending events and restores the public receipt from its saved artifact; an acknowledgment lost before its cursor was saved can
repeat the exact event ID safely. It never reruns inference. A crash before a
terminal event was retained produces cancelled terminal evidence. Errors,
timeouts, and Ctrl-C retain failed/cancelled terminal evidence even if the
relay is unavailable. If accounting cannot be established, this evidence has
`accounting: unavailable` and no invented billable receipt.

A `.state.lock` directory prevents concurrent run/resume on the same journal.
After a hard crash, confirm the original process has stopped before removing
that directory and resuming. A separate lock beside the Mesh owner keystore
serializes this runtime across outputs and models. Timeout/cancellation retains
that node lock: verify the native model is idle, or restart it, before removing
the lock. External clients still require an otherwise idle dedicated Mesh node;
the meter rejects ambiguous concurrent usage.

Cancellation stops the client request, not necessarily native generation; the
provider may continue until its own cancellation support takes effect. The
local answer file is the delivery mechanism for this CLI; remote requester
transport and encrypted delivery are not implemented here. Windows uses the
parent directory's ACL, so choose a private directory.

Checks: `cargo test -p st8-compute-runtime`.
