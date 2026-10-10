using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubImageTests
{
    [Theory, InlineData(false, false, false), InlineData(true, false, false), InlineData(false, true, false), InlineData(false, true, true)]
    public Task ProtectedRustHostTransfersImagesAndPreservesControl(bool direct, bool standard, bool nested)
    {
        var directory = new System.IO.DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !System.IO.File.Exists(System.IO.Path.Combine(directory.FullName, "Cargo.toml"))) directory = directory.Parent;
        var workers = Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS") ?? System.IO.Path.Combine(directory!.FullName, "target", "debug");
        return HubCameraHostFixture.Run(System.IO.Path.Combine(workers, "regain-alpaca.exe"), direct, standard, nested);
    }
    [Fact] public Task NumericTypesAndRankAreLossless() => HubImageFixture.Types();
    [Fact] public Task GroupNumericTypesAndRankReuseLosslessReader() => HubImageFixture.Types(true);
    [Fact] public Task GroupPinsKeepStorageAndSharedBudget() => HubImageFixture.Lifetime(true);
    [Fact] public Task GroupImagesShareCapacityAndReturnItAfterDispose() => HubImageFixture.Capacity(true);
    public static IEnumerable<object[]> GroupFaults => HubImageFixture.GroupFaults.Select(fault => new object[] { fault });
    [Theory, MemberData(nameof(GroupFaults))]
    public Task InvalidGroupTransfersRejectEveryIdentityAndNeverLeak(string fault) => HubImageFixture.Malformed(fault, true);
    [Theory, InlineData(true), InlineData(false)]
    public Task InterruptedGroupReaderClearsPartialPixels(bool cancel) => HubImageFixture.Stalled(cancel, true);
    [Fact] public Task ScalarIntegerRowsPreserveTypesSignsRankAndChunkBoundaries() => HubImageFixture.ScalarRows();
    [Fact] public Task ScalarIntegerConversionRejectsUnrepresentableValuesAndChannels() => HubImageFixture.ScalarRejections();
    [Fact] public Task ReturnedArraysRetainBudgetThroughGcAndRejectCapacityAndCancellation() => HubImageFixture.ArrayLifetime();
    [Fact] public Task PinsKeepImmutableStorageChargedUntilLastReader() => HubImageFixture.Lifetime();
    [Fact] public Task SharedCapacityRejectsBeforePixelAllocationAndReturnsAfterDispose() => HubImageFixture.Capacity();
    public static IEnumerable<object[]> Faults => HubImageFixture.Faults.Select(fault => new object[] { fault });
    [Theory, MemberData(nameof(Faults))]
    public Task InvalidTransfersNeverPublishOrLeak(string fault) => HubImageFixture.Malformed(fault);
    [Fact] public Task CancellationClearsPartialPixelsAndClosesOnlyReader() => HubImageFixture.Stalled(true);
    [Fact] public Task DeadlineClearsPartialPixelsAndClosesOnlyReader() => HubImageFixture.Stalled(false);
}
