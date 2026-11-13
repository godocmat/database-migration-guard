# Independent holdout selection and freeze

The public DMG-02 cases are development and preflight material. Assign one selector outside rule development and two independent labelers with PostgreSQL and adapter or QA expertise. Store the selected cases, labels, and review evidence in a private reviewer-controlled repository through candidate selection.

1. Agree on sample size, strata, and the acceptance criteria with the reviewers. Sample synthetic or approved sanitized cases based on actual migration patterns. Include both adapters and both PostgreSQL majors, all blocking rule families, matched safe and risky controls, ambiguous inputs, and unsupported constructs. Cover DROP TABLE, DROP COLUMN, and TRUNCATE. Assign related migrations, revisions, equivalent adapter copies, and cosmetic variants to one family and one split. Check for overlap with development before labeling.
2. Give both labelers the original files, baseline, declared runner behavior, target, and policy. Each fills out a label independently before viewing Guard or competitor output. Compare labels after both submissions are recorded. Resolve disagreements with a cited primary source, native runner, or disposable two-session probe. Record unresolved ambiguity as an explicit coverage gap and record the gate consequence.
3. Freeze the reconciled labels, evidence, and case bytes before rule tuning. Commit them to the private repository. Record each file's original-byte SHA-256 in a path-sorted UTF-8 digest index with LF line endings, then record the SHA-256 of that index and the commit ID in the freeze receipt. Include the base/head commit IDs or checked-in reproducible recipes, tool and server pins, and the policy digest. Have both reviewers confirm the receipt. Keep the cases and labels sealed through candidate selection.
4. Score the selected build against the frozen set. Require every in-scope destructive case to produce the matching finding or a blocking incomplete result tied to its coverage gap. Count confirmed findings, false positives, missed risks, and incomplete or unsupported cases by rule, adapter, and major. Count blocking incomplete both as its gate result and as a coverage gap. Log any correction with reviewer agreement, freeze a new revision, and rerun the entire set. Move exposed cases to regression coverage and replenish the private holdout before the next tuning cycle.

## Label template

Use one label per variant, adapter, and major. Fill byte offsets against the original file. Use null and provide a reason for unavailable evidence. Record deliberate negative assertions under `absent_rules`.

```yaml
split: holdout
case_id: ""
variant_id: ""
family_id: ""
paired_control_id: ""
stratum: ""  # rule family, destructive form, or coverage gap
data_origin_and_sanitization: ""
inputs:
  base_commit: ""
  candidate_commit: ""
  recipe_or_file_digest_index: ""
  selected_forward_paths: []
  adapter_and_format: ""
  runner_version_and_mode: ""
  postgres_major: null
  postgres_minor_and_image_digest: ""
  policy_sha256: ""
  git_evidence: ""
  applied_history_evidence: null
  seed_and_schema_evidence: ""
expected:
  findings:
    - rule: ""
      severity: ""
      object: ""
      original_path: ""
      start_byte_inclusive: null
      end_byte_exclusive: null
      display_start_and_column_unit: ""
      matched_sql: ""
      trigger_and_evidence: ""
  absent_rules: []
  coverage: ""  # complete or incomplete
  coverage_reason_and_path: ""
  check_exit: null
  gate_reason: ""
review:
  reviewer_and_role: ""
  review_date: ""
  primary_sources: []
  runner_or_server_observations: []  # include pins, SQLSTATE, and resulting state
  unresolved_ambiguity: ""
  reconciliation_record: ""
```

## Freeze receipt

Record the selector and both labelers, private commit and digest-index SHA-256, counts by stratum and adapter/major, family overlap audit, policy and tool pins, freeze date, and reviewer confirmations. At scoring, append the candidate build digest, run IDs, corrections or exposed families, and the next replenishment owner to the review record.
