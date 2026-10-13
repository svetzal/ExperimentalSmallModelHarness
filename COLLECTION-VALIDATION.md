# Original-test protection during collection

Generation52 stopped after its second independent verifier scored 24/24 features
and 2/2 preserved tests. Its frozen coordinator rejected every changed Python
path under tests/, including the newly added test_feature_schema.py. Comparing
all baseline test entries confirmed that no original test was changed.

Future coordinators use the maintained collection check in
benchmarks/collection_validation.py instead of filtering changed path names:

```python
from collection_validation import verify_original_tests

verify_original_tests(workspace, registered_baseline_revision)
```

Make benchmarks/ importable in the coordinator environment and freeze this helper
with the next registration. Call it after collecting the full Git workspace and
verifying its binary patch, before accepting the result or dispatching another
trial. The baseline must be the registered task commit, not an agent-selected
revision. The collected revision defaults to HEAD, matching the committed
workspace used for patch verification. Working-tree inspection and patch checks
remain separate requirements.

The check compares every original tests/ entry's Git mode, type and object ID.
Added tests are allowed. Changed content, deletion, rename, executable-bit changes
and symlink replacement of originals fail. NUL-delimited tree entries preserve
unusual filenames. Invalid revisions and Git failures fail closed; commands have
a finite subprocess timeout. This protects original fixtures and shell tests as
well as Python tests.

Nine real-Git regression cases cover the added-feature-schema-test failure shape,
unchanged tests, changed originals plus additions, deletion, rename, mode changes,
symlink replacement, invalid baseline and tab/newline filenames. Both collected
Generation52 workspaces pass this check read-only. All 154 registered hashes
still match. Generation52 scripts, registration, scores and collection status
remain frozen; this correction does not resume or reclassify that cell.

Run the regression suite with:

```sh
python3 -m unittest discover -s benchmarks -p 'test_collection_validation.py' -v
```
