---
kind: fixed
---
- **A later settings publish no longer undoes a config rollback** (ADR-0246 decision 1). A rollback
  restored the store's tree, but the values the console had written stayed in the settings store, so
  the next settings publish, of any setting, put them back. A rollback now writes each setting the
  restored version runs differently as the store's own value, and the `config.rollback` audit names
  the settings it held and any it could not hold.
