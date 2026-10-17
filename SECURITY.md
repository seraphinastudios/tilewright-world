# Security policy

Report vulnerabilities privately through GitHub's security advisory interface.

Source documents and content packs are untrusted input. Parsers enforce size,
depth and count limits while reading, reject malformed data with stable error
codes, and never execute content. Tools write only to explicit output paths and
never overwrite source files except through validated span edits.
