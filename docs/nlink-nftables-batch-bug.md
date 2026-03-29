# Bug Report: nftables BATCH_BEGIN/END message type constants are wrong

## Summary

The `NFNL_MSG_BATCH_BEGIN` and `NFNL_MSG_BATCH_END` constants in `src/netlink/nftables/mod.rs` have incorrect values. They are left-shifted by 8 bits as if they were subsystem IDs, but they are raw `nlmsg_type` values that should not be shifted. This causes all nftables batch operations to fail with `EINVAL` because the kernel doesn't recognize the batch delimiters.

## Affected version

nlink 0.11.3 (and all prior versions)

## Root cause

In `src/netlink/nftables/mod.rs` (lines 51-53):

```rust
pub const NFNL_MSG_BATCH_BEGIN: u16 = 0x10 << 8;       // = 4096 ← WRONG
pub const NFNL_MSG_BATCH_END: u16 = (0x10 << 8) | 1;   // = 4097 ← WRONG
```

The correct Linux kernel definitions (from `include/uapi/linux/netfilter/nfnetlink.h`):

```c
#define NFNL_MSG_BATCH_BEGIN    NLMSG_MIN_TYPE      // = 0x10 = 16
#define NFNL_MSG_BATCH_END      (NLMSG_MIN_TYPE+1)  // = 0x11 = 17
```

`NLMSG_MIN_TYPE` is `0x10` (16) — a raw netlink message type. It is **not** a subsystem ID. The current code treats it like `nft_msg_type()` which does `(subsystem << 8) | msg`, but `BATCH_BEGIN/END` are nfnetlink-level messages with subsystem `NFNL_SUBSYS_NONE (0)`. Their message type is simply `0x10` / `0x11`.

Because the kernel receives `nlmsg_type = 4096` instead of `16`, it does not recognize the message as a batch delimiter. The inner nftables messages are processed without batch context, and the kernel's `nf_tables_valid_genid()` check fails, returning `EINVAL`.

## Fix

```rust
// Before (wrong):
pub const NFNL_MSG_BATCH_BEGIN: u16 = 0x10 << 8;
pub const NFNL_MSG_BATCH_END: u16 = (0x10 << 8) | 1;

// After (correct):
pub const NFNL_MSG_BATCH_BEGIN: u16 = 0x10;  // NLMSG_MIN_TYPE
pub const NFNL_MSG_BATCH_END: u16 = 0x11;    // NLMSG_MIN_TYPE + 1
```

## How to verify

After the fix, this should succeed:

```rust
use nlink::netlink::{Connection, Nftables};
use nlink::netlink::nftables::types::Family;

let conn = Connection::<Nftables>::new()?;
conn.add_table("test", Family::Inet).await?;
conn.del_table("test", Family::Inet).await?;
```

The `nft` CLI tool (which uses libnftnl and constructs correct batch messages) can serve as a reference — `nft add table inet test` works fine on the same kernel.

## Note

The `Transaction` API is also affected since it uses the same `send_batch()` method with the same wrong constants.
