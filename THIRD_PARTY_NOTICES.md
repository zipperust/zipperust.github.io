# Third-party notices

This repository vendors the third-party code below. Everything else under
`crates/`, `lib/`, and the shell is original to this project (or the
reverse-engineered Zipper runtime).

## fflate v0.8.2

- File: `lib/fflate.js`
- Upstream: https://github.com/101arrowz/fflate
- Source of this copy: the published `fflate@0.8.2` npm tarball, `esm/browser.js`
- License: MIT
- SHA-256: `8cc1f687e0159e977addb6b85e274dbd11e622cf151f4fcb7b85d49622ea43e7`

The file is byte-identical to upstream v0.8.2 `esm/browser.js` (no local edits).
Verify with:

```bash
sha256sum lib/fflate.js
# 8cc1f687e0159e977addb6b85e274dbd11e622cf151f4fcb7b85d49622ea43e7  lib/fflate.js
```

### MIT License

```
MIT License

Copyright (c) 2023 Arjun Barrett

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
