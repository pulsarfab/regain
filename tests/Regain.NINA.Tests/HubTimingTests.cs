using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubTimingTests
{
    [Fact] public Task OnlyCameraAcknowledgementsGainNegotiatedTime() => HubTimingFixture.Semantics();
    public static IEnumerable<object[]> Faults => HubTimingFixture.Faults.Select(value => new object[] { value });
    [Theory, MemberData(nameof(Faults))]
    public Task TimingDescriptorsRequireExactIdentityAndFiniteBound(string fault) => HubTimingFixture.Malformed(fault);
}
