# Third-party licenses

`frame-vocal-pitch` itself is licensed under the Apache License 2.0; see [`LICENSE`](LICENSE).
This file lists everything else that ends up in, or is required by, a release.

Nothing here comes from another pitch-trainer project: no code, assets, sounds or images were
copied from one.

## Contents

- [OpenVR SDK header](#openvr-sdk-header)
- [Inter font](#inter-font)
- [Rust crates](#rust-crates)
- [System libraries (not shipped)](#system-libraries-not-shipped)

## OpenVR SDK header

The OpenVR interface description (`crates/openvr/vendor/openvr_capi.h`, OpenVR SDK v2.15.6,
<https://github.com/ValveSoftware/openvr>) is used to generate the bindings at build time. It is
BSD-3-Clause and is redistributed under the following notice, reproduced verbatim from the SDK:

```
Copyright (c) 2015, Valve Corporation
All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
this list of conditions and the following disclaimer in the documentation and/or
other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its contributors
may be used to endorse or promote products derived from this software without
specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

The OpenVR runtime library itself (`libopenvr_api.so`) is **not** shipped; see
[System libraries](#system-libraries-not-shipped).

## Inter font

The overlay UI renders text with the Inter font, embedded into the binary from
`crates/ui/assets/{Inter-Regular.ttf,Inter-SemiBold.ttf}`. Inter is licensed under the SIL Open
Font License 1.1. The complete license text follows, reproduced verbatim from
`crates/ui/assets/OFL.txt`:

```
Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter)

This Font Software is licensed under the SIL Open Font License, Version 1.1.
This license is copied below, and is also available with a FAQ at:
http://scripts.sil.org/OFL

-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded,
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION AND CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.
```

## Rust crates

The table below is generated from `Cargo.lock` via `cargo metadata`; run
`scripts/gen-third-party-licenses.sh` after changing dependencies (`--check` verifies it in CI).
`Use` is `this project` for the workspace crates, `runtime` for code linked into the released
binary, `dev` for test-only crates and `build` for crates that only run at build time. License
texts are not duplicated here: every crate keeps them in its own package (`~/.cargo/registry/src/`
for registry crates, `crates/openvr/vendor/LICENSE` for the vendored OpenVR header).

<!-- crate-table:begin -->
| Crate | Version | License | Use |
|---|---|---|---|
| `adler2` | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | runtime |
| `aho-corasick` | 1.1.5 | Unlicense OR MIT | build |
| `allocator-api2` | 0.2.21 | MIT OR Apache-2.0 | runtime |
| `annotate-snippets` | 0.11.5 | MIT OR Apache-2.0 | build |
| `anstyle` | 1.0.14 | MIT OR Apache-2.0 | build |
| `arrayref` | 0.3.9 | BSD-2-Clause | runtime |
| `arrayvec` | 0.7.8 | MIT OR Apache-2.0 | runtime |
| `ash` | 0.38.0+1.3.281 | MIT OR Apache-2.0 | runtime |
| `autocfg` | 1.5.1 | Apache-2.0 OR MIT | build |
| `bindgen` | 0.72.1 | BSD-3-Clause | build |
| `bitflags` | 2.13.2 | MIT OR Apache-2.0 | runtime |
| `bytemuck` | 1.25.2 | Zlib OR Apache-2.0 OR MIT | runtime |
| `cc` | 1.10.0 | MIT OR Apache-2.0 | build |
| `cexpr` | 0.6.0 | Apache-2.0/MIT | build |
| `cfg-expr` | 0.20.10 | MIT OR Apache-2.0 | build |
| `cfg-if` | 1.0.5 | MIT OR Apache-2.0 | runtime |
| `cfg_aliases` | 0.2.2 | MIT | build |
| `clang-sys` | 1.9.1 | Apache-2.0 | build |
| `cookie-factory` | 0.3.3 | MIT | runtime |
| `core_maths` | 0.1.1 | MIT | runtime |
| `crc32fast` | 1.5.3 | MIT OR Apache-2.0 | runtime |
| `either` | 1.19.0 | MIT OR Apache-2.0 | build |
| `equivalent` | 1.0.3 | Apache-2.0 OR MIT | runtime |
| `errno` | 0.3.14 | MIT OR Apache-2.0 | runtime |
| `fdeflate` | 0.3.7 | MIT OR Apache-2.0 | runtime |
| `find-msvc-tools` | 0.1.15 | MIT OR Apache-2.0 | build |
| `flate2` | 1.1.10 | MIT OR Apache-2.0 | runtime |
| `foldhash` | 0.1.5 | Zlib | runtime |
| `fontdue` | 0.9.5 | MIT OR Apache-2.0 OR Zlib | runtime |
| `frame-vocal-pitch` | 0.4.0 | Apache-2.0 | this project |
| `fvp-audio` | 0.4.0 | Apache-2.0 | this project |
| `fvp-config` | 0.4.0 | Apache-2.0 | this project |
| `fvp-openvr` | 0.4.0 | Apache-2.0 | this project |
| `fvp-pitch` | 0.4.0 | Apache-2.0 | this project |
| `fvp-ui` | 0.4.0 | Apache-2.0 | this project |
| `glob` | 0.3.4 | MIT OR Apache-2.0 | build |
| `hashbrown` | 0.15.5 | MIT OR Apache-2.0 | runtime |
| `hashbrown` | 0.17.1 | MIT OR Apache-2.0 | runtime |
| `heck` | 0.5.0 | MIT OR Apache-2.0 | build |
| `indexmap` | 2.14.2 | Apache-2.0 OR MIT | runtime |
| `itertools` | 0.13.0 | MIT OR Apache-2.0 | build |
| `itoa` | 1.0.18 | MIT OR Apache-2.0 | runtime |
| `libc` | 0.2.191 | MIT OR Apache-2.0 | runtime |
| `libloading` | 0.8.9 | ISC | runtime |
| `libm` | 0.2.16 | MIT | runtime |
| `libspa` | 0.10.1 | MIT | runtime |
| `libspa-sys` | 0.10.1 | MIT | runtime |
| `linux-raw-sys` | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | runtime |
| `log` | 0.4.34 | MIT OR Apache-2.0 | runtime |
| `memchr` | 2.8.3 | Unlicense OR MIT | runtime |
| `minimal-lexical` | 0.2.1 | MIT/Apache-2.0 | build |
| `miniz_oxide` | 0.8.9 | MIT OR Zlib OR Apache-2.0 | runtime |
| `miniz_oxide` | 0.9.1 | MIT OR Zlib OR Apache-2.0 | runtime |
| `nix` | 0.29.0 | MIT | runtime |
| `nom` | 7.1.3 | MIT | build |
| `nom` | 8.0.0 | MIT | runtime |
| `num-complex` | 0.4.6 | MIT OR Apache-2.0 | runtime |
| `num-integer` | 0.1.47 | MIT OR Apache-2.0 | runtime |
| `num-traits` | 0.2.19 | MIT OR Apache-2.0 | runtime |
| `pipewire` | 0.10.1 | MIT | runtime |
| `pipewire-sys` | 0.10.1 | MIT | runtime |
| `pkg-config` | 0.3.34 | MIT OR Apache-2.0 | build |
| `png` | 0.18.1 | MIT OR Apache-2.0 | runtime |
| `primal-check` | 0.3.4 | MIT OR Apache-2.0 | runtime |
| `proc-macro2` | 1.0.107 | MIT OR Apache-2.0 | runtime |
| `quote` | 1.0.47 | MIT OR Apache-2.0 | runtime |
| `realfft` | 3.5.0 | MIT | runtime |
| `regex` | 1.13.1 | MIT OR Apache-2.0 | build |
| `regex-automata` | 0.4.18 | MIT OR Apache-2.0 | build |
| `regex-syntax` | 0.8.11 | MIT OR Apache-2.0 | build |
| `rtrb` | 0.4.0 | MIT OR Apache-2.0 | runtime |
| `rustc-hash` | 2.1.3 | Apache-2.0 OR MIT | build |
| `rustfft` | 6.4.1 | MIT OR Apache-2.0 | runtime |
| `rustix` | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | runtime |
| `serde` | 1.0.229 | MIT OR Apache-2.0 | runtime |
| `serde_core` | 1.0.229 | MIT OR Apache-2.0 | runtime |
| `serde_derive` | 1.0.229 | MIT OR Apache-2.0 | runtime |
| `serde_json` | 1.0.154 | MIT OR Apache-2.0 | runtime |
| `serde_spanned` | 0.6.9 | MIT OR Apache-2.0 | runtime |
| `serde_spanned` | 1.1.2 | MIT OR Apache-2.0 | build |
| `shlex` | 1.3.0 | MIT OR Apache-2.0 | build |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 | build |
| `signal-hook` | 0.3.18 | Apache-2.0/MIT | runtime |
| `signal-hook-registry` | 1.4.8 | MIT OR Apache-2.0 | runtime |
| `simd-adler32` | 0.3.10 | MIT | runtime |
| `smallvec` | 1.16.4 | MIT OR Apache-2.0 | build |
| `strength_reduce` | 0.2.4 | MIT OR Apache-2.0 | runtime |
| `strict-num` | 0.1.1 | MIT | runtime |
| `syn` | 2.0.119 | MIT OR Apache-2.0 | build |
| `syn` | 3.0.9 | MIT OR Apache-2.0 | runtime |
| `system-deps` | 7.0.8 | MIT OR Apache-2.0 | build |
| `target-lexicon` | 0.13.5 | Apache-2.0 WITH LLVM-exception | build |
| `tiny-skia` | 0.12.0 | BSD-3-Clause | runtime |
| `tiny-skia-path` | 0.12.0 | BSD-3-Clause | runtime |
| `toml` | 0.8.23 | MIT OR Apache-2.0 | runtime |
| `toml` | 1.1.8+spec-1.1.0 | MIT OR Apache-2.0 | build |
| `toml_datetime` | 0.6.11 | MIT OR Apache-2.0 | runtime |
| `toml_datetime` | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 | build |
| `toml_edit` | 0.22.27 | MIT OR Apache-2.0 | runtime |
| `toml_parser` | 1.1.5+spec-1.1.0 | MIT OR Apache-2.0 | build |
| `toml_write` | 0.1.2 | MIT OR Apache-2.0 | runtime |
| `toml_writer` | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 | build |
| `transpose` | 0.2.3 | MIT OR Apache-2.0 | runtime |
| `ttf-parser` | 0.25.1 | MIT OR Apache-2.0 | runtime |
| `unicode-ident` | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | runtime |
| `unicode-width` | 0.2.2 | MIT OR Apache-2.0 | build |
| `version-compare` | 0.2.1 | MIT | build |
| `windows-link` | 0.2.1 | MIT OR Apache-2.0 | runtime |
| `windows-sys` | 0.61.2 | MIT OR Apache-2.0 | runtime |
| `winnow` | 0.7.15 | MIT | runtime |
| `winnow` | 1.0.4 | MIT | build |
| `zlib-rs` | 0.6.8 | Zlib | runtime |
| `zmij` | 1.0.23 | MIT | runtime |
<!-- crate-table:end -->

## System libraries (not shipped)

The release tarball contains one statically linked Rust binary and no shared libraries. These are
loaded from the headset's operating system at runtime and are covered by their own licenses:

| Library | Loaded how | Provided by |
|---|---|---|
| `libopenvr_api.so` | `dlopen` at runtime (SteamVR, `/opt/steamvr/bin/linuxarm64/`) | SteamVR |
| `libpipewire-0.3.so.0` | dynamically linked | SteamOS PipeWire package |
| `libvulkan.so.1` | dynamically linked | SteamOS Vulkan loader package |

`frame-vocal-pitch` does not ship, modify or redistribute any of them, and never restarts
PipeWire or WirePlumber.
