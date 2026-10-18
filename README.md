# devkit

Shared building blocks for new projects.

## `devkit-db`: SQLite now, Postgres later

SeaORM on SQLite for prototypes, with the switch to Postgres kept to a build
feature, a `DATABASE_URL` change and a data copy.

- `devkit_db::connect::<Migrator>(url)`: connect with safe SQLite settings (WAL,
  busy timeout, foreign keys, single in-memory connection) and migrate.
- `devkit_db::is_unique_violation` / `is_foreign_key_violation`: constraint
  errors recognised the same way on both backends.
- `devkit_db::test_support` (feature `test-support`): a fresh database per test
  on SQLite, or on Postgres with `TEST_DB_BACKEND=postgres`.
- `devkit_db::copy::Copy`: move every row from SQLite to Postgres, checked.
- `.github/workflows/db-tests.yml`: reusable CI that runs your database tests on
  both backends.

Setup, the rules for your `CLAUDE.md`, the known backend differences and the
switching runbook: [docs/sqlite-to-postgres.md](docs/sqlite-to-postgres.md).

## Development

```bash
cargo test -p devkit-db
```

On Postgres, see the local commands in the doc above. Tag releases (`v0.1.1`)
and depend on tags, never on `main`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this
repository, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.
