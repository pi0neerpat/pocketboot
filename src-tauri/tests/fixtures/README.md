# dfu-util transcript fixtures

**These transcripts are synthetic.** They were written by hand from dfu-util
0.11 source wording, not captured from a real RadioMaster Pocket. Replace each
one with a real capture once Q5 hardware captures exist (Definition of Done).

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
