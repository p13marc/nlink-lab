# Bug Report: nftables mutation operations fail with EINVAL — missing batch wrapping

## Summary

All nftables mutation methods (`add_table`, `add_chain`, `add_rule`, `del_table`, `del_chain`, `del_rule`) fail with `EINVAL` (os error 22) on modern Linux kernels because they send standalone netlink messages without the required `NFNL_MSG_BATCH_BEGIN` / `NFNL_MSG_BATCH_END` wrapping.

## Affected version

nlink 0.11.2 (and likely all prior versions)

## Affected code

All nftables mutation methods in `src/netlink/nftables/connection.rs` use `nft_request_ack()` (line 477) which sends the message directly as a single netlink message:

| Method | Line | Sends via |
|--------|------|-----------|
| `add_table` | 42 | `nft_request_ack` |
| `add_chain` | 118 | `nft_request_ack` |
| `add_rule` | 210 | `nft_request_ack` |
| `del_table` | 81 | `nft_request_ack` |
| `del_chain` | 142 | `nft_request_ack` |
| `del_rule` | ~240 | `nft_request_ack` |

The `Transaction` API (line 396) and `send_batch()` (line 411) correctly wrap messages in batch begin/end, but they are not used by the individual methods.

## Root cause

Since approximately Linux 4.6, the kernel's nftables subsystem requires all mutation operations to be sent within a netfilter batch (`NFNL_MSG_BATCH_BEGIN` / `NFNL_MSG_BATCH_END`). The kernel's `nf_tables_valid_genid()` check fails for non-batched mutation messages, returning `EINVAL`.

The `nft_request_ack()` method (line 477) sends raw netlink messages:

```rust
async fn nft_request_ack(&self, mut builder: MessageBuilder) -> Result<()> {
    let seq = self.socket().next_seq();
    builder.set_seq(seq);
    builder.set_pid(self.socket().pid());
    let msg = builder.finish();
    self.socket().send(&msg).await?;      // ← sent without batch wrapping
    // ... wait for ACK
}
```

Compare with `send_batch()` (line 411) which correctly wraps:

```rust
async fn send_batch(&self, messages: Vec<Vec<u8>>) -> Result<()> {
    let mut batch = Vec::new();

    // NFNL_MSG_BATCH_BEGIN
    let mut begin = MessageBuilder::new(NFNL_MSG_BATCH_BEGIN, NLM_F_REQUEST);
    // ... append nfgenmsg ...
    batch.extend_from_slice(&begin.finish());

    // Add all messages
    for msg_data in &messages {
        batch.extend_from_slice(msg_data);
    }

    // NFNL_MSG_BATCH_END
    let mut end = MessageBuilder::new(NFNL_MSG_BATCH_END, NLM_F_REQUEST);
    // ... append nfgenmsg ...
    batch.extend_from_slice(&end.finish());

    self.socket().send(&batch).await?;
    // ... wait for ACK
}
```

## How to reproduce

```rust
use nlink::netlink::nftables::types::Family;

// Create a connection in any network namespace
let conn: Connection<Nftables> = namespace::connection_for("myns")?;

// This fails with EINVAL on modern kernels
conn.add_table("test", Family::Inet).await?;
```

## Observed error

```
failed to create nftables table on 'server': kernel error: Invalid argument (os error 22) (errno 22)
```

## Fix

Route all nftables mutation methods through `send_batch()` instead of `nft_request_ack()`. Each individual mutation should wrap its single message in a one-element batch.

For example, `add_table` should become:

```rust
pub async fn add_table(&self, name: &str, family: Family) -> Result<()> {
    if name.is_empty() || name.len() > 256 {
        return Err(Error::InvalidMessage(
            "table name must be 1-256 characters".into(),
        ));
    }

    let mut builder = MessageBuilder::new(
        nft_msg_type(NFT_MSG_NEWTABLE),
        NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL,
    );
    let nfgenmsg = NfGenMsg::new(family);
    builder.append(&nfgenmsg);
    builder.append_attr_str(NFTA_TABLE_NAME, name);

    let seq = self.socket().next_seq();
    builder.set_seq(seq);
    builder.set_pid(self.socket().pid());

    self.send_batch(vec![builder.finish()]).await   // ← wrap in batch
}
```

Apply the same pattern to all other mutation methods (`add_chain`, `add_rule`, `del_table`, `del_chain`, `del_rule`).

Alternatively, `nft_request_ack` itself could be modified to always wrap the message in a batch, since all callers are mutation operations.

## Note on the `Transaction` API

The existing `Transaction` builder API works correctly because `Transaction::commit()` calls `send_batch()`. This bug only affects the individual convenience methods.

## Compatibility

The batch wrapping is backwards-compatible with older kernels — the batch protocol has been supported since Linux 3.13 (the initial nftables release). There is no downside to always using batch wrapping.
