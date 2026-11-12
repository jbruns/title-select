# 16. Header Rules come from the DAT, and headers are never stripped

Date: 2026-11-11

## Status

Accepted

## Context

For NES, FDS, Atari 7800 and Lynx, No-Intro hashes each ROM without its header, but most dumps carry one, so a
headered dump audits as Mismatched or Unmatched. The header can't be rebuilt from a headerless DAT, and many emulators
need it: NES and 7800 games rarely play without it.

## Decision

- A DAT Revision's Header Rule is the one its DAT names (`<clrmamepro header="…"/>`). A DAT that names none gets no
  Header Rule, whatever its system. Title Select has four built-in rules, matching No-Intro's detectors: NES
  (`NES\x1a`, 16 bytes), FDS (`FDS\x1a`, 16 bytes), Atari 7800 (`ATARI7800` at offset 1, 128 bytes) and Lynx
  (`LYNX`, 64 bytes).
- When a DAT Revision has a Header Rule and an Entry starts with that Header, the Audit hashes the Entry once, without
  its Header, and matches that. Otherwise it hashes the whole Entry as before. There is no second hash. An archive's
  recorded CRC covers the Header, so a headered member is always read, never matched on claims.
- Evidence records the Header's size. A headered Entry that matches is Matched, not a separate Finding.
- No Repair or Build ever strips a Header: both copy an Entry's bytes unchanged and verify them against the hashes
  without the Header.
- For NES and 7800, where emulators generally need the Header, a Matched Entry without one carries a warning that the
  Game may not play. The warning is a note on the Game, not a Game Status, and never stops a Build.

## Considered Options

- Detecting a header by system name: wrong for a DAT that is itself headered, and the DAT already says.
- Hashing every candidate both with and without its header: no better result, at the cost of a second hash.
- Stripping Headers in Repairs so a Source Set matches its DAT byte for byte: destroys bytes no DAT can restore.
