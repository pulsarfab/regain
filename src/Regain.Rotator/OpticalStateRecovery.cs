using System.IO;

namespace Regain.Rotator;

/// Read recovery only. A write with a lost acknowledgement is never replayed.
internal sealed class OpticalStateRecovery<T> where T : class
{
    private readonly string instance = Guid.NewGuid().ToString("N");
    private long generation;
    internal string ConnectionEpoch => instance + ":" + generation;
    private T? baseline;
    private Exception? blocked;
    internal bool CanRecover => baseline is not null && blocked is null;
    internal bool IsBlocked => blocked is not null;
    internal void Reset() { baseline = null; blocked = null; generation++; }
    internal void RequireVerified() {
        if (blocked is not null) throw new IOException("Optical recovery is blocked; explicitly disconnect and reconnect after checking the device.", blocked);
    }
    internal void Writing() { RequireVerified(); baseline = null; generation++; }
    internal void Uncertain(Exception error) { baseline = null; blocked = error; }

    internal T Read(Func<T> read, Action reopen, Action close, Func<T, bool> idle,
        Func<T, T, bool> matches, Action<string> log, Action? backoff = null)
    {
        RequireVerified();
        T value;
        try { value = read(); }
        catch (Exception first) when (first is IOException or TimeoutException or InvalidOperationException) {
            generation++;
            var expected = baseline;
            if (expected is null) {
                Uncertain(first);
                throw new IOException("USB connection failed with unknown or moving optical state; no movement was replayed. Check the device and reconnect.", first);
            }
            Exception failure = first;
            for (int attempt = 1; attempt <= 3; attempt++) {
                try {
                    close();
                    log($"Spontaneous USB failure: reopening the same selected device ({attempt}/3); no command is replayed.");
                    reopen(); // The session verifies the hardware serial before any status is accepted.
                    value = read();
                } catch (Exception error) {
                    failure = error;
                    if (attempt < 3) { if (backoff is null) Thread.Sleep(2000); else backoff(); }
                    continue;
                }
                if (!idle(value) || !matches(expected, value)) {
                    Uncertain(new IOException("Optical state changed or is unavailable after USB reconnection."));
                    throw new IOException("Optical state changed or is unavailable after USB reconnection; no movement was replayed. Check the device and reconnect.", blocked);
                }
                baseline = value;
                log("USB connection restored; hardware identity and idle optical state verified.");
                return value;
            }
            Uncertain(failure);
            throw new IOException("USB reconnection failed after 3 attempts; no movement was replayed. Check the device and reconnect.", failure);
        }
        baseline = idle(value) ? value : null;
        return value;
    }
}

/// A fresh driver read, rather than NINA's cached Connected flag and position.
public interface IVerifiedOpticalDevice
{
    bool OpticalConnectionRequested { get; }
    string OpticalConnectionEpoch { get; }
    string ReadVerifiedOpticalState();
    bool OpticalStatesMatch(string before, string after);
}
