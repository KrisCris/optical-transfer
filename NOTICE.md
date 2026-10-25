# Notices

This project began as a Rust port of Decimen Optical Transfer v0.5.3, pinned
at commit `5301fc2020716b5a97d4edf6e1793c3dedbf6e17`, from
https://github.com/bashalarmistalt/decimen-optical-transfer.

Decimen Optical Transfer is Copyright (c) 2026 Evan Crawley (Bash Alarmist)
and is licensed under the GNU Affero General Public License, version 3 or
later (AGPL-3.0-or-later). This derivative project is also licensed
AGPL-3.0-or-later. The complete corresponding source must be made available
with distributed builds.

The upstream project incorporates zxing-cpp
(https://github.com/zxing-cpp/zxing-cpp), licensed under the Apache License,
Version 2.0. Any vendored zxing-cpp artifacts must retain their accompanying
Apache-2.0 license and notices.

The macOS desktop build compiles zxing-cpp 3.1.1 from source (through the
`zxing-cpp` crate) and links it statically for its Receive mode. It is used
unmodified under the same Apache License, Version 2.0; every release archive
carries that license as `web/vendor/decimen-codec/LICENSE.zxing-cpp`.

Portions contributed upstream by Steve Dakh are incorporated under their MIT
License terms; see the pinned upstream NOTICE for the full attribution.

Wire v4's fountain code is the `raptorq` crate 2.0.1
(https://github.com/cberner/raptorq), licensed under the Apache License,
Version 2.0, and compiled into every build unmodified.
