# Third-party notices

`bx` is MIT licensed — see [LICENSE](LICENSE). It also contains code adapted
from the project below, whose licence requires its copyright notice to travel
with the code.

## MXC

<https://github.com/microsoft/mxc>

The sandbox *plan generators* are adapted from MXC: `sandbox::seatbelt`
(macOS Seatbelt profile text), `sandbox::bwrap` (Linux bubblewrap argv), and
`sandbox::appcontainer` (Windows AppContainer plan). `sandbox::Policy` is a
trimmed projection of MXC's `ContainerPolicy`. These are vendored by hand
rather than depended on, so they do not appear in `Cargo.toml` — which is
exactly why the notice belongs here.

bx does *not* use MXC's execution path; see the `## Sandboxing` section of
`CLAUDE.md` for why (it would break MCP's raw-stdio contract).

    MIT License

    Copyright (c) Microsoft Corporation.

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
