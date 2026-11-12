# 17. A Game's status counts only its own files

Date: 2026-11-11

## Status

Accepted

## Context

Redump DATs list many byte-identical tracks under different Games. At the PlayStation acceptance, 62 Games audited as
Incomplete because one track matched a file inside another Game's ZIP: per ROM it was accurate, but nothing about the
Game itself was present or playable. Game Status is meant to tell the Operator what they can play.

## Decision

- An Entry inside a container named for another Game (that Game's ZIP or folder) does not make a ROM present for this
  Game. It makes the ROM Available Elsewhere.
- Game Status counts only present ROMs. A Game whose ROMs are all absent or Available Elsewhere is Missing. The Audit
  reports Available Elsewhere ROMs with the Game, and a Repair can still fill the Game from them.
- Per-Entry Findings and Evidence are unchanged: this is about how Entries count towards Games.

A clone's parent's container counts as the clone's own. Merged sets keep a clone's files in its parent's ZIP, and
split sets keep the files a clone shares with its parent there, so treating that container as another Game's would
make every complete clone in such a set Missing. The DAT's `cloneof` names the parent.
