namespace Regain.Core;

/// <summary>Retry attempts for the current/latest capture and the last observed failure in this connection.</summary>
public sealed record CameraRetryStatus(int Recaptures = 0, int Downloads = 0, int UsbReads = 0, string? LastFailure = null)
{
    public long Count => (long)Recaptures + Downloads + UsbReads;
    public string DriverInfo(string phase) =>
        $"state: {Short(phase, 28)}; retries: {Count}; last: {Short(LastFailure ?? "none", 40)}";

    // Full failure text and per-kind counts remain in RetryStatus/diagnostics and
    // logs. NINA displays DriverInfo in a narrow, single-line field.
    private static string Short(string text, int limit)
    {
        var clean = string.Join(" ", text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
        return clean.Length <= limit ? clean : clean[..(limit - 1)] + "…";
    }
}
