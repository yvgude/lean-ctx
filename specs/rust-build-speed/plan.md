# Plan

1. Capture the existing release build with Cargo timings in the shared target.
2. Add the development profile, installation selection, help and behavioral tests.
3. Preserve the local compiler memory gate while allowing incremental workspace
   builds to bypass sccache and dependencies to continue using its cache.
4. Add pinned target-aware release caching and remove duplicate host test runs.
5. Apply persistent development-host CPU budgets and conservative defaults for future
   Cargo invocations; leave active jobs and memory untouched.
6. Correct build-script invalidation and exercise freshness plus the source
   contamination guard with a real offline Cargo fixture.
7. Independently review the integrated diff, compare builds and run quality gates.

Owner: orchestration lead; research/review workers are read-only.
