# grim-engine

A from-scratch reimplementation, in Rust, of the engine needed to run **Harry Potter and the
Chamber of Secrets** (PC, 2002), which was built on a modified Unreal Engine 1.

The goal is to play the original game on modern systems using only its data files. No code
or binaries from the original game or from other engine reimplementations are used, and this
repository contains no game content: you need your own copy of the game.

## Status

**Work in progress.** Nothing is playable yet. The first phase, reading every package of the
game and decoding its assets (textures, sounds, meshes, animations, levels and scripts), is
done. The runtime comes next.

## Getting started

```sh
tools/extract_assets.sh /path/to/your/game-disc.bin   # unpacks the data into game/ (not tracked)
cargo run --release -p grim-viewer                    # browse the assets at http://127.0.0.1:8765/
```

## License

GPL-3.0. See [LICENSE](LICENSE).
