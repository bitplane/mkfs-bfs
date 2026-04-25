# mkfs.bfs

Create SCO BFS (Boot File System) images in Rust.

BFS is the tiny flat filesystem SCO UNIX used to hold standalone boot
programs. It has no subdirectories and no indirect blocks: each file is a
contiguous run of 512-byte blocks. Linux can read it via the `bfs` kernel
module.

## Install

From crates.io:

```bash
cargo install mkfs-bfs
```

This installs a binary called `mkfs.bfs`. Pre-built binaries for Linux
(x86_64 / aarch64), macOS (universal), and Windows are also attached to
each [GitHub release][releases].

[releases]: https://github.com/bitplane/mkfs-bfs/releases

## Usage

```bash
# Create an empty 4 MB image.
mkfs.bfs out.bfs 4

# Populate a new image from a directory tree (subdirectories are walked
# recursively and files are flattened into the BFS root, since BFS has
# no subdirectory support).
mkfs.bfs -d ./payload out.bfs 4

# Mount on Linux (the kernel module is `bfs`).
sudo mount -o loop,ro -t bfs out.bfs /mnt
```

## Limits

- Up to 503 user files (504 inodes including root).
- 14-byte filenames; longer names are truncated with a warning.
- Files are stored contiguously, so the largest file is bounded by the
  largest free run, not by total free space.

## License

WTFPL with one extra clause: don't blame me. See [LICENSE](LICENSE).
