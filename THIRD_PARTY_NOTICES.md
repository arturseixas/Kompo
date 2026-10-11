# Third-party code

Kompo vendors source from two open-source projects by the ArtCraft Team and
contributors, both licensed **MIT OR Apache-2.0** (at the user's option). The
license texts and the upstream `NOTICE` files are kept next to the code.

| Directory | Upstream | What Kompo uses |
|---|---|---|
| `third_party/effectcraft/` | https://github.com/storytold/effectcraft | `effects` (300+ CPU effects and transitions), `keyframe`, `raster`, `project`, `color`, `geom`, `time`, `segment`, `track`, `path`, `text` |
| `third_party/filmcraft/` | https://github.com/storytold/filmcraft | `audio-dsp` (EQ, dynamics, noise reduction, EBU R128 loudness), `scopes` (waveform, vectorscope, histogram, parade), `color` |

Files are unmodified copies except for the root `Cargo.toml` of each tree (workspace members trimmed) and the fonts in
`third_party/effectcraft/assets/fonts` (SIL OFL-1.1, see the `OFL-*.txt` files there).
The ArtCraft name, wordmark and logos are trademarks of the ArtCraft Team and are not part of what is vendored.

The vendored trees are separate Cargo workspaces (`exclude`d from the root one), so Kompo's lints and formatting do not apply to them.
