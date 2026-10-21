# dfu-util transcript fixtures

The happy upload and download transcripts contain output captured from a
successful RadioMaster Pocket session. The other transcripts remain synthetic,
based on dfu-util 0.11 source wording; replace them with real captures before
release (Q5 / Definition of Done).

The diagnostics report records output but not process exit codes. The upload
fixture uses exit 0, consistent with the app accepting the backup. The
download fixture retains exit 74 to exercise the expected post-leave path; that
exit code was not captured in the report.

Format:

- The first line is `# exit: <code>`, the process exit code to replay.
- Every other line is dfu-util output, verbatim. Progress redraws keep their
  literal `\r` characters, so the parser's `\r`/`\n` splitting is exercised.

| File | Invocation | Expected outcome |
|---|---|---|
| `happy-upload.txt` | upload | ok (`UploadDone`) |
| `happy-download-leave.txt` | download + leave | success candidate despite exit 74 (R8) |
| `first-contact-dfuerror.txt` | upload | ok; `dfuERROR` and suffix warnings are benign |
| `unplug-mid-upload.txt` | upload | no device (R21, back to `waiting`) |
| `unplug-mid-download.txt` | download + leave | `deviceGone` |
| `busy-device.txt` | upload | `busy` |
