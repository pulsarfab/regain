namespace Regain.Rotator;

/// Per-client leases for a single serial worker in an ASCOM local server.
public sealed class SharedAccessoryDevice(AccessorySession session, Action<AccessorySession, Func<bool>> showSetup)
{
    public object Gate { get; } = new();
    public AccessorySession Session { get; } = session;
    private readonly HashSet<Guid> clients = [];
    private bool setupActive;
    public bool Connected(Guid id) { lock (Gate) return clients.Contains(id) && Session.Connected; }
    public void Connect(Guid id, bool value)
    {
        lock (Gate) {
            if (value) { if (!Session.Connected) { clients.Clear(); Session.Connect(); } clients.Add(id); }
            else { clients.Remove(id); if (clients.Count == 0 && !setupActive) Session.Disconnect(); }
        }
    }
    public void Setup()
    {
        lock (Gate) { if (setupActive) throw new InvalidOperationException("Setup is already open"); setupActive = true; }
        try { showSetup(Session, () => { lock (Gate) return clients.Count > 0; }); }
        finally { lock (Gate) { setupActive = false; if (clients.Count == 0) Session.Disconnect(); } }
    }
}
