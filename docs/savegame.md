# Savegame format

Everything a save slot of the original game contains, byte by byte, so that grim-engine can read
the game's saves and write saves the game can read.

Sources: the console/script code of the game's own packages (`Engine.u`, `HGame.u`, `Default.ini`)
and six original slots (six independent savegame archives: 6 `Save0.usa`, 6 `Save0.bmp`,
61 `<Map>_pa.usa` with 3333 actor records in total). Nothing here is taken from another
implementation. Statements that the bytes support only indirectly are marked **hypothesis**.

## 1. Where saves live

`System/Default.ini` sets the paths:

| key | value | meaning |
|-----|-------|---------|
| `SavePath` | `../Save` | directory that holds the slots |
| `SaveExt` | `usa` | extension of a saved package |
| `Paths` | `../save/*.usa` | saved packages are searched there too |
| `UseSaveSlot` | `0` | slot the game starts on |
| `AutoSave`, `AutoSaveTimeMinutes`, `AutoSaveIndex` | `False`, `5`, `6` | unused by this game |

A slot is a **directory** `Save/Slot<n>/`. Saving is `SaveGame <n>` on the console; loading adds
`?load=<n>` to the map URL. The files inside a slot are always called `Save0.*` whatever the slot
number is: the number is only in the directory name.

A slot contains:

```
Save/Slot<n>/
  Save0.usa           the level the player is standing in, as a UE1 package
  Save0.bmp           120x90 thumbnail for the load menu
  cache/              empty in all six slots
  <Map>_pa.usa        one per level visited in this game: its persistent actors
```

Who writes what: `SmartStart.bDoLevelSave` makes `harry.TravelPostAccept` call `SaveGame` right
after a level change, which is why a slot accumulates one `_pa` file per level visited.
`HPConsole.doLevelSave(int i)` writes the slot summary (section 5).

## 2. `Save0.usa` — the current level

A plain UE1 package, nothing custom: the same reader that opens a `.unr` opens it.

| offset | field | value in the samples |
|--------|-------|----------------------|
| 0x00 | tag | `0x9E2A83C1` |
| 0x04 | version / licensee | 79 / 0 |
| 0x08 | package flags | `0x0001` |
| 0x0C | name count / offset | 3998 / 0x40 |
| 0x14 | export count / offset | 2629 / 0x359ADC |
| 0x1C | import count / offset | 623 / 0x35828A |
| 0x24 | guid | 16 bytes |
| 0x34 | generation count | 1, followed by (export count, name count) |

The package holds a **complete copy of the level**, not a diff:

- the `Level` export, named `MyLevel`, last in the table (35 200 bytes in the sample),
- one export per actor, with all its properties, including the player (the `harry` actor with its
  inventory, spell book, current game state and status), the `GameInfo`, the status manager and the
  status items that carry beans, stars and house points,
- the `Brush`, `Model` and `Polys` objects of the movers,
- `LevelInfo`, whose `LevelEnterText` is the map file name (`<Map>.unr`).

Imports point at the code and texture packages (`Engine`, `HGame`, `HP2_Master`, the map's own
texture packages...). The map package itself is **not** imported: the level travelled into the save.

Loading a slot therefore means opening `Save0.usa` instead of the map, which is what grim-engine
already does (`grim-game <path to Save0.usa>`).

## 3. `Save0.bmp` — the thumbnail

A standard bottom-up 8-bit BMP, 11 878 bytes: `BM`, file size, pixel data at offset 1078,
`BITMAPINFOHEADER` of 40 bytes, 120 x 90, 1 plane, 8 bpp, no compression, image size 10 800,
2835 pixels/metre (72 dpi) on both axes, 256 palette entries used (256 `RGBQUAD` at offset 54).

## 4. `cache/`

An empty directory in all six slots. UE1 keeps downloaded packages in a cache next to a save;
a single-player game has none. **Hypothesis**: it only needs to exist, and may not even be needed.

## 5. `GameSaveInfo` — the slot summary for the menu

`Engine.GameSaveInfo` is a native class with five fields, in declaration order:

| field | type |
|-------|------|
| `numBeans` | int |
| `numStars` | int |
| `numPoints` | int |
| `savePointID` | int |
| `currentLevelString` | string |

Its own script says every object of the class is serialized one field at a time by a function in
the engine (`SerializeInfo`), and that new fields have to be appended there — so the file is the
five values in this order and nothing else.

`HPConsole.doLevelSave(int i)` builds it: it pauses the game, takes `LevelInfo.LevelEnterText` cut
at the `.` (so `Entryhall_hub`) into `currentLevelString`, asks the player for
`FindNearestSavePointID()` into `savePointID`, and calls the `Actor` native
`SaveGameSaveInfo("GameSaveInfo" $ i, obj)` (native index 325; the counterpart is
`LoadGameSaveInfo`, native 326).

The first argument is a *name*, `GameSaveInfo0`, `GameSaveInfo1`, ..., not a directory. **No such
file exists inside the six slot directories**, so the engine writes it somewhere else, most likely
directly under `Save/`; the archives we have only kept the slot directory. Its exact path,
extension and byte layout are therefore **unknown** — we have no sample.

It is not needed to restore a game: the counters live in the level snapshot (the status actors
inside `Save0.usa`). `GameSaveInfo` only feeds the load menu.

## 6. `<Map>_pa.usa` — persistent actors of visited levels

One file per level visited in this game, named after the map (`Entryhall_hub_pa.usa`). Despite the
`.usa` extension it is **not** a package: it is a custom stream that starts with `PA0`. All
integers are little-endian.

### 6.1 The two string encodings

- **Counted UTF-16**: `u32` byte count, then that many bytes of UTF-16LE **including the
  terminating NUL** (so the count is bytes, twice the character count). Used for the magic, the
  level name, actor names, class names, property names, struct type names and the value of a `name`
  property.
- **Compact-index text** (UE1's `FString`): a UE1 compact index, then, if it is positive, that many
  bytes of 8-bit text (NUL included), or, if it is negative, that many UTF-16 code units. Used only
  for the value of a `str`/`string` property. Both branches occur in the samples (5 Unicode values
  out of 40 837 strings).
- UE1 compact index, for reference: in the first byte bit 7 is the sign, bit 6 says another byte
  follows and bits 0-5 are the lowest bits; in each following byte bit 7 says another byte follows
  and bits 0-6 are the next bits.

### 6.2 File layout

```
"PA0"        8 bytes, UTF-16LE with its NUL and *without* a count prefix
levelName    counted UTF-16, e.g. "EntryHall_Hub.unr"
i32          -1   in every sample      hypothesis: format version
i32           0   in every sample      unknown
i32           0   in every sample      unknown
i32 count    number of actor records
record * count
"EOF"        counted UTF-16 (12 bytes); the file ends here
```

The level name is the map file name, but its capitalisation does not always match the file's
(`EntryHall_Hub.unr` for `Entryhall_hub.unr`): compare case-insensitively.

### 6.3 A record

```
name      counted UTF-16    the actor's object name, e.g. "Jellybean0"
class     counted UTF-16    its class name, without package
u32 size  byte size of the property block
block     size bytes: tagged properties, ending with a property named "None"
```

`size` is exact: in all 3333 records the `None` that closes the block falls precisely on
`size` bytes.

### 6.4 A tagged property

```
name             counted UTF-16; the name "None" closes the block and nothing follows it
u8 info          bits 0-3 type, bits 4-6 size code, bit 7: the value of a bool, or
                 "an array index follows" for every other type
structTypeName   counted UTF-16, only for type 10
size             only for size codes 5, 6 and 7: u8, u16, i32
arrayIndex       only when bit 7 is set and the type is not bool: compact index
value            per the type table below
```

Size codes 0 to 4 mean 1, 2, 4, 12 and 16 bytes; 5, 6 and 7 mean the explicit `u8`, `u16` or `i32`
above. Code 7 never appears in the six saves. Array indices are always a single byte below 128 in
the samples, so the multi-byte forms are read as UE1 encodes them but are **untested**.

**The size does not say how many bytes the value takes.** It is the size UE1's own table gives (the
length UE1 *would* have written), while this writer writes something else: an object or class
reference writes nothing at all yet counts as 1 byte, and a `name` writes a whole counted string yet
also counts as 1. Two verified examples: a `PointRegion` struct declares 6 and writes 5 (its object
field writes nothing), and an `Animations` struct declares 4 and writes four counted strings. A
reader must decode by type and ignore the size. **Hypothesis** for the cause: the tag size comes
from a different code path than the value.

| type | name | value |
|------|------|-------|
| 1 | byte | 1 byte |
| 2 | int | 4 bytes |
| 3 | bool | nothing: the value is bit 7 of `info`; the size code is always 5 with an explicit size of 0 |
| 4 | float | 4 bytes |
| 5 | object | nothing at all: the reference is not saved |
| 6 | name | counted UTF-16 |
| 8 | class | nothing at all |
| 10 | struct | struct type name, then the payload of section 6.5 |
| 13 | str | compact-index text |

Types 7 (`string`), 9 (`array`), 11 (`vector`), 12 (`rotator`), 14 (`map`) and 15 (`fixedarray`)
never appear: vectors and rotators travel as structs.

### 6.5 Struct payloads

A struct is written field by field, with the same rules per field type and **no tags at all**, so a
reader needs the layout of every struct type it may meet. Those seen in the six saves:

| struct | declared size | bytes written |
|--------|---------------|----------------|
| `Vector` | 12 | 3 floats |
| `Rotator` | 12 | 3 ints |
| `Plane` | 16 | 4 floats |
| `Color` | 4 | 4 bytes |
| `Scale` | 17 | vector, float, byte |
| `BoundingBox` | 25 | 2 vectors, byte |
| `PointRegion` | 6 | object (nothing), int, byte: 5 bytes |
| `MaxMin` | 8 | 2 ints |
| `Sounds` | 3 | three object fields: nothing |
| `Animations` | 4 | four `name` fields: four counted strings |
| `TVendorDialog` | 15 | as the game declares it |

The table above is what the six saves happen to use; a reader does not need it, since the fields
come from the struct's own declaration in the packages, which is how grim-engine decodes them.

### 6.6 Which actors are in, and what of them

- One record per actor whose `bPersistent` is true when the level is left. `bPersistent` is
  `Engine.Actor`'s, an addition of this game to UE1. In practice: collectibles, chests, cauldrons,
  secret-area markers, cutscenes already played, spawners, a few movers and triggers, and some
  NPCs. Between 3 and 161 records per level in the saves we have.
- A record carries the **whole** property set of the actor, not only what changed: one entry per
  slot of every property of its class chain (a static array contributes one entry per element),
  which is 285 to 619 entries per record. The only ones left out are those the engine owns,
  flagged native (`0x1000`) or transient (`0x2000`), and dynamic arrays. Checked on all 3333
  records of the six saves: the entries of every one of them are exactly the properties its class
  declares minus those. The only dynamic array in play is `Actor.AuxAnims`; that arrays are left
  out on purpose is a **hypothesis** resting on it.
- Order: the most derived class first and then up the chain, in declaration order inside each class.
- **Hypothesis**: on re-entering the level the game loads the map as usual and applies each record
  to the actor with the same object name.

## 7. What grim-engine does today

- Reads `Save0.usa` as a package and plays it: the level, its actors and the player's state come up
  (`grim-play <path to Save0.usa>`). Original saves work.
- Takes in the slot's `_pa` files when it opens a save, so walking back into a level the saved game
  had been in finds it as that game left it.
- Keeps every level it leaves in the same form, so a level revisited in the same session is found
  the way it was left: what was collected stays collected.
- Ignores `Save0.bmp` and `cache/`.
- Writes nothing yet; there is no `SaveGame` command. `grim-save` can already write a `_pa` stream
  byte for byte, so what is missing to save a game is the package **writer** for `Save0.usa` (the
  whole level, which we only read today), the 120x90 thumbnail and the `GameSaveInfo` file once we
  know where it goes.

## 8. Open points

- `GameSaveInfo`: the path and layout of the file the native writes. No sample.
- The three header words `-1, 0, 0` of a `_pa` file: constant in all 61 files, meaning unverified.
- Multi-byte array indices in a property tag: never seen, read as UE1 encodes them.
- Whether the writer skips any property of a class: in every record we have, all of them are there.

## 9. Validation

- 6 slots, 61 `_pa` files, 3333 records: every property block is consumed to the byte, the record
  count matches the header count, and each file ends exactly at its `EOF` marker with no bytes left
  over.
- The six `Save0.usa` open with our package reader and one of them runs and renders in the engine.
