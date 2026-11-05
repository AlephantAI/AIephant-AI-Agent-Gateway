# Added and adapted code notices

The repository's existing `LICENSE` remains GNU GPL version 3.

The pure validators in `ai-gateway/src/validation/{mod,header,image,select,strict_json}.rs` and their media regression fixtures are adapted from the `openmodels-gateway` package, source baseline `42e793a` (2026-11-05). Its manifest declares `Apache-2.0` and authors `legendqi <legendqi@alephant.ai>` and `Alephant Developers`. Adaptations integrate Alephant configuration, headers, Tools and dispatcher behavior; source database-contract loading and billing/rule execution are excluded. A copy of Apache License 2.0 is included here. Original module source carried no additional copyright notices.

The byte-preserving multipart and heartbeat implementations adapt protocol ideas to Alephant's existing transport and error types.

Added image decoding dependencies retain their upstream notices alongside this file:

- `png` 0.18.1: MIT OR Apache-2.0.
- `zune-core` 0.5.3 and `zune-jpeg` 0.5.15: MIT OR Apache-2.0 OR Zlib.
- `image` 0.25.10: test fixture encoder only, MIT OR Apache-2.0; it is not a runtime image decoder in this gateway.

License identifiers above were checked against the cached package manifests used for verification. Dependency license files are copied verbatim from those packages. The source archive replacement and local proxy used during verification are not shipped dependencies.
