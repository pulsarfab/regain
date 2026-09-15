using System.Collections;
using System.Diagnostics;
using System.Runtime.InteropServices;
using ASCOM.Alpaca.Clients;
using ASCOM.Common.Alpaca;
using ASCOM.DeviceInterface;

[assembly: ComVisible(false)]
namespace ZwoGain.Ascom;

[ComVisible(false)]
public abstract partial class CameraBase : ICameraV4, IDisposable
{
    static CameraBase() => Dependencies.Install();
    private readonly int slot;
    private AlpacaCamera? client;
    private bool disposed;
    private readonly object sync = new();
    protected CameraBase(int slot) { this.slot = slot; }
    protected AlpacaCamera Client
    {
        get
        {
            lock (sync)
            {
                if (disposed) throw new ObjectDisposedException(nameof(CameraBase));
                if (client is null)
                {
                    var settings = ServerSettings.Load();
                    settings.EnsureServer();
                    client = new AlpacaCamera(new AlpacaConfiguration {
                        ServiceType = ServiceType.Http, IpAddressString = settings.Address, PortNumber = settings.Port,
                        RemoteDeviceNumber = slot, ClientNumber = unchecked((uint)Guid.NewGuid().GetHashCode()),
                        EstablishConnectionTimeout = 10, StandardDeviceResponseTimeout = 120, LongDeviceResponseTimeout = 120,
                        ImageArrayTransferType = ImageArrayTransferType.ImageBytes, NumberOfRetries = 0
                    });
                }
                return client;
            }
        }
    }
    public string Name => $"ZWOgain Retryable Camera {slot + 1}";
    public string Description => "ZWO camera driver with automatic retries";
    public string DriverInfo => "ZWOgain ASCOM frontend for the Rust Alpaca server";
    public string DriverVersion => typeof(CameraBase).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 4;
    public ArrayList SupportedActions => new() { "ZwoGain.Diagnostics", "ZwoGain.Controls", "ZwoGain.SetControl" };
    public bool Connected { get => client?.Connected ?? false; set { if (value) Client.Connected = true; else if (client is not null) client.Connected = false; } }
    public void Connect() => Client.Connect();
    public void Disconnect() { if (client is not null) client.Disconnect(); }
    public void SetupDialog()
    {
        Exception? failure = null;
        var thread = new Thread(() => {
            try
            {
                var settings = ServerSettings.Load();
                using var dialog = new SetupForm(settings, slot);
                if (dialog.ShowDialog() == System.Windows.Forms.DialogResult.OK)
                {
                    lock (sync)
                    {
                        if (client?.Connected == true) throw new ASCOM.InvalidOperationException("Disconnect before changing the server address");
                        settings.Save(); client?.Dispose(); client = null;
                    }
                }
            }
            catch (Exception e) { failure = e; }
        });
        thread.SetApartmentState(ApartmentState.STA); thread.Start(); thread.Join();
        if (failure is not null) throw new ASCOM.DriverException(failure.Message, failure);
    }
    public IStateValueCollection DeviceState => new StateValueCollection(Client.DeviceState.Select(v => new StateValue(v.Name, v.Value)).ToList());
    public string Action(string ActionName, string ActionParameters) => Client.Action(ActionName, ActionParameters);
    public void CommandBlind(string Command, bool Raw) => Client.CommandBlind(Command, Raw);
    public bool CommandBool(string Command, bool Raw) => Client.CommandBool(Command, Raw);
    public string CommandString(string Command, bool Raw) => Client.CommandString(Command, Raw);
    public void AbortExposure() => Client.AbortExposure();
    public void StopExposure() => Client.StopExposure();
    public void StartExposure(double Duration, bool Light) => Client.StartExposure(Duration, Light);
    public void PulseGuide(GuideDirections Direction, int Duration) => Client.PulseGuide((ASCOM.Common.DeviceInterfaces.GuideDirection)Direction, Duration);
    public void Dispose()
    {
        AlpacaCamera? closing;
        lock (sync) { if (disposed) return; disposed = true; closing = client; client = null; }
        try { if (closing is not null) { try { closing.Connected = false; } finally { closing.Dispose(); } } }
        finally { GC.SuppressFinalize(this); }
    }
    ~CameraBase() { Task.Run(() => { try { Dispose(); } catch { } }); }
}

[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A1"), ProgId("ASCOM.ZWOgain.Camera1"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera1() : CameraBase(0);
[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A2"), ProgId("ASCOM.ZWOgain.Camera2"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera2() : CameraBase(1);
[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A3"), ProgId("ASCOM.ZWOgain.Camera3"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera3() : CameraBase(2);
[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A4"), ProgId("ASCOM.ZWOgain.Camera4"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera4() : CameraBase(3);
