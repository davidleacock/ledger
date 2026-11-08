# ledger

A streaming payments engine. It reads a CSV of transactions (deposits,
withdrawals, disputes, resolves, chargebacks), applies them to client
accounts, and writes the final account states as CSV to stdout.

## Usage

```sh
cargo run -- transactions.csv > accounts.csv
```

- **stdout** gets only the account CSV, sorted by client ID, with amounts to
  4 decimal places.
- **stderr** gets diagnostics: one `skipped: …` line per malformed row
  (with its line number), one `rejected: …` line per transaction the ledger
  refused (with its tx ID and the reason), and any fatal error.
- **Exit code** is `0` if the input was processed, even when some rows were
  skipped or rejected, and `1` for a usage error, an unreadable file, an I/O
  failure partway through the input, or a failure writing output.

## Architecture

```
src/
  lib.rs              run(): reader → ledger → writer, with logging and error policy
  main.rs             CLI entry point: argument handling, opens the file, exit codes
  domain/
    amount.rs         Amount: fixed-precision (4dp) signed money value
    transaction.rs    Transaction enum, ClientId/TxId, DisputeState machine
    account.rs        Account: balances, lock flag, atomic balance operations
  csv_io.rs           TransactionReader (streaming CSV → Transaction), write_accounts
  ledger.rs           Ledger: applies transactions to accounts, tracks deposits
```

### Responsibilities

| Layer        | Enforces |
|--------------|----------|
| `csv_io`     | CSV syntax, known `type` values, u16/u32 IDs, amount present for deposit/withdrawal |
| `Amount`     | ≤ 4 decimal places, overflow-checked arithmetic |
| `Transaction`| deposit/withdrawal amounts are strictly positive; legal dispute-state transitions |
| `Account`    | sufficient funds, locked accounts reject operations, atomic updates |
| `Ledger`     | tx lookup, client matching, duplicate tx IDs, dispute state per deposit |

## Design decisions & trade-offs

### Business rules / assumptions

Where the specification is silent we chose behaviour that makes sense for a bank:

1. **Only deposits can be disputed.** The spec's dispute mechanics (available
   decreases, held increases, chargeback removes funds) only make sense for a
   credit. Disputes referencing a withdrawal are ignored. As a side effect,
   only deposits need to be remembered, which reduces memory.
2. **Locked accounts reject everything.** After a chargeback the account is
   frozen. All later deposits, withdrawals, disputes, resolves, and
   chargebacks for that client are ignored.
3. **Available funds may go negative.** If a client deposits, withdraws, and then
   disputes the deposit, the full amount is still held and `available` becomes
   negative. This is the fraud pattern the engine is meant to catch, and the
   negative balance shows the debt honestly instead of hiding it.
4. **The dispute lifecycle is one-way.**
   `Undisputed → Disputed → Resolved | ChargedBack`. Both endings are terminal,
   so a resolved transaction cannot be disputed again. This prevents endless
   dispute/resolve cycles.
5. Dispute, resolve, and chargeback must name the same client as the original
   deposit, or they are ignored.
6. A withdrawal with insufficient *available* funds fails and leaves the
   account unchanged. Held funds cannot be withdrawn.
7. Errors on the partner's side (unknown tx, wrong dispute state, mismatched
   client) are ignored and do not stop processing.
8. **Deposit and withdrawal tx IDs must be globally unique.** A second deposit
   or withdrawal with an ID already seen (in either kind) is rejected as a
   duplicate.
9. **A failed deposit or withdrawal still uses up its ID.** The partner issued
   it, so a later row with the same ID is a replay, not a new request. This
   keeps the books consistent and makes replays easy to spot as possible
   fraud. A failed deposit can never be disputed.
10. **Only deposits and withdrawals create accounts**, as the spec says ("if a
    client doesn't exist create a new record"). A withdrawal as a client's
    first transaction is still rejected for insufficient funds, but the
    client appears in the output with zero balances. Disputes, resolves, and
    chargebacks refer to a transaction that must already exist, so they never
    create an account. A rejected duplicate doesn't create one either.

### Type-level decisions

- **`Amount` is a newtype over `rust_decimal::Decimal`.** Decimal arithmetic avoids
  binary floating-point error. The newtype hides the raw arithmetic
  operators and exposes only `checked_add` / `checked_sub`, so overflow is
  always handled explicitly and can never panic or wrap silently.
- **`Amount` is signed.** Rule 3 needs negative balances. That transaction
  amounts must be strictly positive is checked once, in
  `Transaction::deposit` / `Transaction::withdrawal`.
- **Inputs with more than 4 decimal places are rejected, not rounded.** Changing
  a monetary value without telling anyone is a worse failure than refusing a
  malformed row. Trailing zeros are ignored, so `1.50000` is accepted.
  Output always shows exactly 4 decimal places.
- **`Transaction` is an enum with per-variant fields.** A dispute with an amount,
  or a deposit without one, cannot be represented. The CSV layer's job is to
  turn a loose raw row into this strict type, so the engine never sees
  malformed data.
- **The dispute lifecycle lives in `DisputeState`**, with transition methods that
  return `Result`. Rule 4 is enforced and unit-tested on its own, separate
  from the engine. A failed transition reports which step was attempted as an
  `Action` enum (`Dispute` / `Resolve` / `Chargeback`), not a string, so
  matching on errors is checked by the compiler and a typo won't compile.
- **`Account::total` is computed** (`available + held`), not stored, so the
  spec's invariant `total = available + held` holds by construction.
- **`Account` operations are atomic.** Each one computes all new values before
  writing any of them, so a failed operation (locked, insufficient funds,
  overflow) leaves the account exactly as it was. `release` and `chargeback`
  also refuse to take out more than is currently held, as a defensive check on
  the ledger's bookkeeping.
- **`ClientId` / `TxId` are type aliases (`u16` / `u32`), not newtypes.** This
  keeps the CSV mapping and the code simple. The trade-off is that the
  compiler won't catch a client ID swapped for a tx ID. The two have
  different widths, which catches most mix-ups.

### CSV input / output

- **Streaming.** `TransactionReader` is an iterator that reads one row at a
  time into a single reused buffer, so memory use for reading stays the same
  however big the file is. Memory then depends only on how many accounts and
  deposits the ledger has to remember.
- **Generic over `io::Read` / `io::Write`.** The reader takes any byte source
  and `write_accounts` takes any writer. The CLI uses a file and stdout, tests
  use in-memory buffers, and a server could pass a TCP stream unchanged.
- **Lenient on layout, strict on content.** Whitespace around fields is
  trimmed, blank lines are skipped, and dispute/resolve/chargeback rows may
  leave the `amount` column empty or out entirely. Values must still be
  exact: `type` must be lowercase and one of the five known kinds, `client`
  must fit in u16 and `tx` in u32.
- **An amount on dispute/resolve/chargeback rows is ignored.** These rows always
  act on the original deposit's amount, so an extra value can't change the
  result. Rejecting the row would lose a real dispute over a column that
  doesn't matter.
- **Errors are per row and don't end the stream.** A malformed row yields `Err`
  for that row, with the line number, and the next call carries on with the
  next row. The reader doesn't decide whether to skip or abort; the caller
  does. The exception is an I/O error on the underlying input: the reader
  reports it once and then stops, so a caller that skips bad rows can't loop
  forever.
- **A raw row is validated into a `Transaction` in one place.** Serde turns the
  row into a private `RawRecord` that matches the file's columns. A
  `TryFrom<RawRecord> for Transaction` impl then applies the domain rules, so
  invalid data never gets past this module. The domain types don't depend on
  serde.
- **Output always has 4 decimal places** (`1.5000`), using `Amount`'s
  `Display`. Rows are written in the order the caller gives them.

### Ledger

```rust
accounts:       BTreeMap<ClientId, Account>     // ≤ 65,536 entries, sorted
deposits:       HashMap<TxId, StoredDeposit>    // successful deposits: client, amount, dispute state
non_disputable: HashSet<TxId>                   // withdrawals + failed deposits
```

- **Only what disputes need is stored.** Input rows aren't kept. Successful
  deposits are kept with their client, amount, and dispute state, because a
  later dispute refers to them by ID. That's about 32–40 bytes each including
  hash table overhead.
- **Duplicate detection uses a separate `HashSet` of non-disputable IDs**
  (withdrawals and failed deposits), at about 5–9 bytes each. Together with
  the deposits map this catches every duplicate. It also lets a dispute on a
  withdrawal be reported as `NotDisputable` rather than `UnknownTx`.
  Alternatives considered:
  - *Check only the deposits map.* No extra memory, but misses every duplicate
    that involves a withdrawal.
  - *One combined map for deposits and withdrawals.* One lookup instead of
    two, but each withdrawal entry would be sized for a deposit, about 4–6×
    the memory.
  - *A bit array over the whole u32 range.* A fixed 512 MiB, which only pays
    off with hundreds of millions of transactions.
  - *A compressed bitmap (`roaring`).* Best memory at any scale, but an extra
    dependency.

  The check lives in a single private method (`ensure_new_tx`), so it can be
  swapped for a bitmap later if inputs ever approach the full u32 range. At
  that scale the deposits map is the real memory limit anyway.
- **Accounts are kept in a `BTreeMap`**, so output is sorted by client ID with
  no separate sort step. Output is the same on every run and easy to compare
  in tests. With at most 65,536 clients the O(log n) lookup is negligible.
- **Each rejection has its own error variant** (`DuplicateTx`, `UnknownTx`,
  `NotDisputable`, `ClientMismatch`, `InvalidTransition`, `Account`), each
  carrying the tx ID. The caller can log a useful reason and tests can assert
  the exact reason.
- **Dispute steps are atomic.** The ledger first checks that the state
  transition is legal, then applies the account change, and only records the
  new dispute state once the account change has succeeded. A step rejected
  because the account is locked leaves both the account and the deposit's
  state unchanged.
- **Concurrency (not implemented; design notes).** `Ledger` is a plain
  single-threaded struct. To handle many concurrent streams you could split
  the work by client ID, since one client's transactions never touch another
  client's account. Two things would need deciding:
  - *Global tx ID uniqueness.* Duplicate detection crosses clients, so the
    workers would need either a shared concurrent ID set (a lock-sharded set,
    or a concurrent map such as `dashmap`), or the business would have to
    accept a weaker rule, such as IDs unique per client.
  - *Ordering.* The input is in global chronological order. Once it's split,
    rows going to different workers can be processed out of order relative
    to each other. That's fine as long as **each client's** transactions stay
    in order: every rule (balances, disputes, locking) concerns one client,
    so only per-client order affects the result. Routing every client to one
    worker over an in-order channel guarantees it. The one exception is
    duplicate detection across clients: which of two rows with the same ID
    counts as "first" may then depend on timing.

### Error handling & CLI

- **`run(input, output, log)` in `lib.rs` contains all the processing.** It's
  generic over `Read` / `Write`, so the whole pipeline, including the error
  policy, is unit-tested with in-memory buffers. `main.rs` only parses the
  argument, opens the file, picks stdout/stderr, and maps the result to an
  exit code.
- **Bad data is skipped; broken infrastructure is fatal.** A malformed row or
  rejected transaction is a problem on the partner's side: it's logged and
  processing carries on. An I/O error on the input means we can't know what
  we missed, so the run stops and **writes no account output**, because a
  partial set of balances would look valid but be wrong.
- **Writing a log line can never fail the run.** Diagnostics are best-effort.
  stderr is wrapped in a `BufWriter`, so logging every rejection stays cheap
  on large inputs.
- **No `unsafe`, and no panics on input data.** The only `expect` /
  `unreachable!` calls guard internal invariants, such as "a stored deposit
  always has an account", never input values.

### Performance

On a laptop, a randomly generated 5,000,000-row file (146 MB; 60% deposits,
30% withdrawals, 10% dispute/resolve/chargeback, all 65,536 clients) runs in
about **2.1 s with ~207 MB peak memory** (release build). Memory is mostly the
~3M stored deposits. Input rows are streamed and never kept.

## Testing

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Unit tests live next to the code in `#[cfg(test)]` modules:

- `amount`: parsing, the precision limit, display formatting, negative values,
  overflow.
- `transaction`: positive-amount validation, accessors, every valid and invalid
  dispute-state transition.
- `account`: each operation, insufficient funds, holds that push available
  negative, chargeback locking, every operation on a locked account, overflow,
  and checks that failed operations change nothing.
- `csv_io`: the spec's example, whitespace handling, the amount column empty,
  missing, or filled in on dispute rows, maximum IDs, empty and header-only
  input, blank lines, each kind of invalid row (with line numbers), reading
  continuing after bad rows, I/O errors ending the stream, and output
  formatting and order.
- `ledger`: the spec's example, sorted output, a first-event withdrawal, every
  combination of duplicate deposit/withdrawal IDs, failed transactions using
  up their IDs, dispute → resolve and dispute → chargeback, available going
  negative, several disputes open at once, unknown, wrong-client, and
  non-disputable references, every invalid dispute transition, locked
  accounts rejecting everything, and rejected steps leaving dispute state
  unchanged.
- `lib` (`run`): sorted output, skipped and rejected rows logged while
  processing continues, I/O failure partway through (fatal, no output),
  unreadable header, output write failure.

End-to-end tests (`tests/cli.rs`) run the compiled binary on each sample in
`tests/fixtures/<name>/input.csv` and compare stdout exactly with
`expected.csv`. They also check stderr and the CLI error cases (no argument,
too many arguments, missing file, a directory instead of a file).

| Fixture | Covers |
|---|---|
| `basic` | whitespace, unordered clients, insufficient funds, a first-event withdrawal creating an empty account |
| `dispute_resolve` | holds blocking withdrawals, resolve, re-dispute after resolve rejected, resolve without dispute |
| `chargeback_locks` | chargeback locking, every later transaction rejected, other clients unaffected |
| `dispute_after_withdrawal` | available going negative, debt left after chargeback |
| `duplicates` | every duplicate-ID combination, failed withdrawal using up its ID, withdrawal not disputable |
| `malformed_rows` | each kind of bad row skipped and logged, wrong-client dispute, amount on a dispute row ignored |
| `precision` | 4dp arithmetic with no float error, trailing zeros, large values |
