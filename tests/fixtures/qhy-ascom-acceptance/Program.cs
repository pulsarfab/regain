using System.Diagnostics;
using System.Security.Cryptography;
using Regain.Ascom;

if (Environment.GetEnvironmentVariable("REGAIN_TEST_QHY600_HARDWARE") != "1" ||
    string.IsNullOrEmpty(Environment.GetEnvironmentVariable("REGAIN_ASCOM_PROFILES")))
    throw new InvalidOperationException("Explicit QHY600 opt-in and isolated ASCOM profiles are required");
_ = Task.Run(async () => { await Task.Delay(TimeSpan.FromMinutes(3)); Environment.Exit(124); });
if (Environment.GetEnvironmentVariable("REGAIN_TEST_QHY_USB_WHEEL") == "1") {
    using var first = new QhyUsbFilterWheel(); using var second = new QhyUsbFilterWheel();
    try {
        first.Connected=true; second.Connected=true;
        if(first.Names.Length!=9 || second.FocusOffsets.Length!=9) throw new Exception("Unexpected slots");
        foreach(int target in Enumerable.Range(0,9).Concat(Enumerable.Range(0,9).Reverse()).Concat(new[]{8,0,4,4,0})) {
            first.Position=(short)target;var clock=Stopwatch.StartNew();
            while(second.Position==-1) {if(clock.Elapsed.TotalSeconds>45) throw new TimeoutException();Thread.Sleep(100);}
            if(second.Position!=target) throw new Exception("Wrong shared position");
            Console.WriteLine($"ASCOM USB slot {target}: {clock.Elapsed.TotalSeconds:F3}s");
        }
        first.Connected=false;
        if(!second.Connected || second.Position!=0) throw new Exception("Second lease lost");
        second.Connected=false; first.Connected=true;
        if(first.Position!=0) throw new Exception("Reopen lost position");
        if(first.SupportedActions.Contains("Regain.Calibrate")) throw new Exception("Unsupported calibration advertised");
        Console.WriteLine("USB ASCOM all slots, shared client leases and reconnect passed");
    } finally {first.Connected=false;second.Connected=false;}
    return;
}
using var camera = new Camera1();
using var wheel = Environment.GetEnvironmentVariable("REGAIN_TEST_QHY_WHEEL") == "1" ? new QhyFilterWheel1() : null;
void Move(int position) {
    wheel!.Position = checked((short)position); var clock = Stopwatch.StartNew();
    while (wheel.Position == -1) { if(clock.Elapsed.TotalSeconds>45) throw new TimeoutException("Wheel did not settle"); Thread.Sleep(150); }
    if(wheel.Position != position) throw new Exception("Wrong wheel readback");
    Console.WriteLine($"ASCOM wheel settled at {position}");
}
try {
    if (wheel is not null) { wheel.Connected = true; if(wheel.Names.Length != 9 || wheel.FocusOffsets.Length != 9) throw new Exception("Wrong wheel geometry"); }
    camera.Connected = true;
    if (wheel is not null) for(int position=0;position<9;position++) Move(position);
    if(camera.Name != "QHY600M" || camera.CameraXSize != 9600 || camera.CameraYSize != 6422 || camera.ExposureMax != 3600)
        throw new Exception("Unexpected ASCOM identity or exposure limits");
    camera.BinX = camera.BinY = 1; camera.NumX = 9600; camera.NumY = 6422;
    camera.Gain = 30; camera.Offset = 30;
    if(wheel is not null) { camera.SetCCDTemperature=-5; camera.CoolerOn=true; }
    var hashes = new HashSet<string>();
    for(int index=0;index<3;index++) {
        if(wheel is not null) Move(index*4);
        var clock=Stopwatch.StartNew(); camera.StartExposure(5,false);
        if(wheel is not null) {
            try { wheel.Position=1; throw new Exception("Move accepted during capture"); }
            catch(ASCOM.InvalidOperationException) { Console.WriteLine("ASCOM move rejected during capture"); }
        }
        while(!camera.ImageReady) { if(clock.Elapsed.TotalSeconds>40) throw new TimeoutException(); Thread.Sleep(100); }
        var image=(int[,])camera.ImageArray;
        if(image.GetLength(0)!=9600 || image.GetLength(1)!=6422) throw new Exception("ASCOM transposed/truncated image");
        var data=new byte[Buffer.ByteLength(image)]; Buffer.BlockCopy(image,0,data,0,data.Length);
        using var hash=SHA256.Create(); string digest=BitConverter.ToString(hash.ComputeHash(data)).Replace("-","");
        if(!hashes.Add(digest)) throw new Exception("Stale ASCOM image");
        Console.WriteLine($"Full ASCOM frame {index}: 9600x6422, {clock.Elapsed.TotalSeconds:F2}s, SHA256 {digest}");
    }
    if(wheel is not null) {
        camera.Connected=false;
        if(!wheel.Connected) throw new Exception("Camera disconnect lost wheel lease");
        Move(0); camera.Connected=true;
        if(camera.CoolerOn) throw new Exception("Cooler enable was not cleared when last camera lease disconnected");
        // QHY's measured PWM can ramp down after the acknowledged zero demand.
        // Do not fabricate instantaneous zero power in the driver or test.
        Console.WriteLine($"Cooler disabled after camera lease release; measured ramp-down power {camera.CoolerPower}%");
        wheel.Connected=false;
        if(!camera.Connected) throw new Exception("Wheel disconnect lost camera lease");
        camera.StartExposure(5,false); var clock=Stopwatch.StartNew();
        while(!camera.ImageReady) { if(clock.Elapsed.TotalSeconds>40) throw new TimeoutException(); Thread.Sleep(100); }
        var final=(int[,])camera.ImageArray;
        if(final.GetLength(0)!=9600 || final.GetLength(1)!=6422) throw new Exception("Invalid frame after lease changes");
        Console.WriteLine("Full ASCOM frame after independent camera/wheel reconnects passed");
    }
    Console.WriteLine(camera.DriverInfo);
} finally { camera.Connected=false; if(wheel is not null) wheel.Connected=false; }
