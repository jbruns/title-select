# 7. Builds write TorrentZip, validated structurally

Date: 2026-10-30

## Status

Accepted

## Context

A Build writes one ZIP per Game. ROM managers such as RomVault, clrmamepro and igir treat TorrentZip as the standard
for such ZIPs. They validate it structurally: the member order, the fixed timestamp, the flags, and a CRC-32 of the
central directory carried in the `TORRENTZIPPED-` comment. They never recompress to compare deflate bytes. Matching
the original tool byte for byte needs its exact zlib version (1.1.3). The one Rust TorrentZip crate found is new,
lacks ZIP64 and sets no UTF-8 flag.

## Decision

- Every ZIP a Build writes is TorrentZip, produced by Title Select's own writer and checked by its own validator,
  following the format as igir documents it. The goal is output that the validators ROM managers use accept.
- Deflate uses a pinned, statically linked zlib at level 9. Byte-identity with other TorrentZip tools is a best
  effort, checked against a committed golden made from a synthetic payload, and is not guaranteed.
- A source ZIP is copied unchanged only when its members are exactly the Game's ROMs under their DAT names, each
  payload matches, and it is already valid TorrentZip. Anything else is repacked from the verified payloads.

## Consequences

- Builds from a Source Set that is not TorrentZipped repack every Game, so they get no block cloning until a repair
  TorrentZips the Source Set.
- Extracted output, disc layouts and CHD or RVZ are separate decisions, left to a later spike.

## Amendment (2026-11-02, #46)

- The Storage Policy decides, per system, whether a Play Library stores each Game as TorrentZip or loose (ADR 0010).
  Every ZIP a Build writes is still TorrentZip, and a loose file is its one ROM under the DAT's name.
