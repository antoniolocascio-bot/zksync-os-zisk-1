# Historical wire fixtures

These fixtures pin inputs emitted before wire v5 existed. They are stored as
hex so schema changes remain ordinary reviewable text diffs; tests remove
whitespace and decode them before execution.

| Fixture | Provenance | Spec | Framed SHA-256 | Historical commitment |
|---|---|---|---|---|
| `wire-v3-session-batch-1.hex` | PR #18 commit `25085ac`, `gen_session_inputs`, batch 1 | AtlasV2 / minor 30 | `cd7213bd6fbc745787212cc09df350da1bfcd30b199121e37899e16400f4d195` | `0x0b34f54c4edd64be7d7b6bcfa09edddc22a7b87e14a253191e8fd521f087914a` |
| `wire-v3-atlas-v3.hex` | The unchanged wire-v3 library at PR #18 commit `25085ac`, existing `coinbase_reward_is_full_effective_gas_price` honest input | AtlasV3 / minor 31 | `3c373b2040f1cf7960c3baeaa741f0a7fbf878c109b70f47e1dc9ddb0e9967e1` | `0x4e101c4291eebeb63b52cd9be88da5a9ab03307500663aab4805aad5da7e4cca` |

Both files use the ZiSK stdin frame `[wire length: u64 LE][bincode][zero
padding to 8 bytes]`. The regression tests check exact v3 decode/re-encode,
normalization, collecting execution, streaming guest execution, and the pinned
commitment.
