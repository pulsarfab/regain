namespace Regain.Core;

/// <summary>Retry attempts for the current/latest capture and the last observed failure in this connection.</summary>
public sealed record CameraRetryStatus(int Recaptures = 0, int Downloads = 0, int UsbReads = 0, string? LastFailure = null)
{
    public long Count => (long)Recaptures + Downloads + UsbReads;
    public string DriverInfo(string phase) =>
        $"state: {phase}; retries: {Count} (recaptures: {Recaptures}, downloads: {Downloads}, USB reads: {UsbReads}); last failure: " +
        string.Join(" ", (LastFailure ?? "none").Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
}
