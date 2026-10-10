namespace Regain.Hub;

internal static class HubAlpacaIdentity
{
    // Match uuid::Uuid's accepted catalog forms, without trimming opaque IDs
    // or accepting .NET-only X formatting. Other strings remain case-sensitive.
    internal static string Normalize(string identity)
    {
        var text = identity;
        if (text.Length == 45 && text.StartsWith("urn:uuid:", StringComparison.Ordinal)) text = text.Substring(9);
        else if (text.Length == 38 && text[0] == '{' && text[37] == '}') text = text.Substring(1, 36);
        var format = text.Length == 32 ? "N" : text.Length == 36 ? "D" : null;
        return format is not null && Guid.TryParseExact(text, format, out var id) ? id.ToString() : identity;
    }
}
