using Xunit;

// These process integration tests exercise short I/O deadlines. Concurrent
// worker startup can starve timer callbacks on small Windows CI runners.
[assembly: CollectionBehavior(DisableTestParallelization = true)]
