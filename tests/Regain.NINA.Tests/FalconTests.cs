using System.Text.Json;
using Xunit;
using Regain.Rotator;

namespace Regain.NINA.Tests;

public class FalconTests
{
    [Fact]
    public async Task NativeAdapterMovesSyncsCancelsAndReopensSelectedDevice()
    {
        var root = new DirectoryInfo(AppContext.BaseDirectory);
        while (root is not null && !File.Exists(Path.Combine(root.FullName,"Cargo.toml"))) root=root.Parent;
        string worker=Path.Combine(root!.FullName,"target","debug","regain-device.exe");
        Assert.True(File.Exists(worker));
        string folder=Path.Combine(Path.GetTempPath(),"regain-falcon-nina-"+Guid.NewGuid().ToString("N"));
        string profile=Path.Combine(folder,"falcon.json");
        string[] keys=["REGAIN_FALCON_WORKER","REGAIN_ROTATOR_SETTINGS","REGAIN_ROTATOR_SIMULATE"];
        var old=keys.Select(Environment.GetEnvironmentVariable).ToArray();
        string? hardware=Environment.GetEnvironmentVariable("REGAIN_TEST_FALCON_SERIAL");
        try {
            Environment.SetEnvironmentVariable(keys[0],worker);
            Environment.SetEnvironmentVariable(keys[1],profile);
            Environment.SetEnvironmentVariable(keys[2],hardware is null ? "1" : null);
            Directory.CreateDirectory(folder);
            if(hardware is not null) File.WriteAllText(profile,JsonSerializer.Serialize(new {Serial=hardware}));
            using var device=new CaaRotator(true);
            Assert.True(await device.Connect(CancellationToken.None));
            try {
                Assert.False(device.IsMoving);
                float initial=device.MechanicalPosition;
                device.Reverse=false;
                device.Sync(42);
                Assert.InRange(device.Position,41.95f,42.05f);
                await device.Move(5,CancellationToken.None);
                Assert.InRange(device.Position,46.95f,47.05f);
                device.Reverse=true;
                Assert.InRange(device.Position,46.95f,47.05f);
                await device.MoveAbsolute(48,CancellationToken.None);
                Assert.InRange(device.Position,47.95f,48.05f);
                using var cancelled=new CancellationTokenSource(); cancelled.Cancel();
                await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>device.Move(90,cancelled.Token));
                Assert.False(device.IsMoving);
                device.Reverse=false;
                await device.MoveAbsoluteMechanical(initial,CancellationToken.None);
                device.Halt();
                device.Sync(initial);
                device.Disconnect();
                Assert.True(await device.Connect(CancellationToken.None));
                Assert.InRange(device.MechanicalPosition,initial-.05f,initial+.05f);
                Assert.Equal(hardware ?? "FALCON-SIMULATION",JsonSerializer.Deserialize<CaaProfile>(File.ReadAllText(profile))!.Serial);
            } finally { device.Disconnect(); }
        } finally {
            for(int i=0;i<keys.Length;i++) Environment.SetEnvironmentVariable(keys[i],old[i]);
        }
    }
}
