using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class OpticalStateRecoveryTests
{
    private sealed record State(string Serial, int Position, bool Moving = false);

    [Fact]
    public void ReopensAndVerifiesIdleStateAfterSpontaneousReadFailure()
    {
        var guard = new OpticalStateRecovery<State>();
        State state = new("selected-serial", 3);
        bool removed = false;
        int opens = 0, closes = 0;
        State Read() => removed ? throw new IOException("HID removed") : state;
        State Poll() => guard.Read(Read, () => { opens++; removed = false; }, () => closes++, s => !s.Moving,
            (a, b) => a == b, _ => { }, () => { });
        Assert.Equal(state, Poll()); removed = true;
        Assert.Equal(state, Poll()); Assert.Equal(1, opens); Assert.Equal(1, closes);
    }

    [Theory]
    [InlineData("another-serial", 3, false)]
    [InlineData("selected-serial", 0, false)]
    [InlineData("selected-serial", 3, true)]
    public void DifferentIdentityPositionOrMotionPermanentlyBlocksAutomaticContinuation(string serial, int position, bool moving)
    {
        var guard = new OpticalStateRecovery<State>();
        State state = new("selected-serial", 3);
        bool removed = false;
        int opens = 0;
        State Poll() => guard.Read(() => removed ? throw new IOException("removed") : state,
            () => { opens++; removed = false; state = new(serial, position, moving); }, () => { },
            s => !s.Moving, (a, b) => a == b, _ => { }, () => { });
        _ = Poll(); removed = true;
        Assert.Contains("Optical state", Assert.Throws<IOException>(() => Poll()).Message);
        Assert.Throws<IOException>(() => Poll()); Assert.Throws<IOException>(guard.Writing);
        Assert.Equal(1, opens);
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void MovingOrUnknownStateNeverReopensOrReplays(bool moving)
    {
        var guard = new OpticalStateRecovery<State>();
        int opens = 0;
        if (moving) _ = guard.Read(() => new State("selected", 3, true), () => { }, () => { }, s => !s.Moving,
            (a, b) => a == b, _ => { });
        Assert.Throws<IOException>(() => guard.Read(() => throw new IOException("removed"), () => opens++, () => { },
            s => !s.Moving, (a, b) => a == b, _ => { }));
        Assert.Equal(0, opens);
    }

    [Fact]
    public void LostWriteAcknowledgementRequiresExplicitReconnectEvenIfReadsWouldSucceed()
    {
        var guard = new OpticalStateRecovery<State>();
        guard.Writing(); guard.Uncertain(new IOException("lost move acknowledgement"));
        int reads = 0;
        Assert.Throws<IOException>(() => guard.Read(() => { reads++; return new State("same", 3); },
            () => { }, () => { }, _ => true, (a, b) => a == b, _ => { }));
        Assert.Equal(0, reads);
        guard.Reset(); guard.RequireVerified();
    }

    [Fact]
    public void ReconnectAttemptsAreBoundedAndFailureRemainsLatched()
    {
        var guard = new OpticalStateRecovery<State>();
        bool removed = false;
        int opens = 0, backoffs = 0;
        State Poll() => guard.Read(() => removed ? throw new IOException("removed") : new State("same", 3),
            () => { opens++; throw new IOException("still absent"); }, () => { }, _ => true, (a, b) => a == b,
            _ => { }, () => backoffs++);
        _ = Poll(); removed = true;
        Assert.Throws<IOException>(() => Poll()); Assert.Equal(3, opens); Assert.Equal(2, backoffs);
        removed = false; Assert.Throws<IOException>(() => Poll()); Assert.Equal(3, opens);
    }
}
