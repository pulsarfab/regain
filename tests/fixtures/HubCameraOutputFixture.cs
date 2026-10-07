using System.Diagnostics;
using System.IO;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;

namespace Regain.TestFixtures;

internal static class HubCameraOutputFixture
{
    private static void Check(bool value, string message) { if (!value) throw new InvalidOperationException(message); }
    private static void Failure<T>(Action action) where T : Exception {
        try { action(); throw new InvalidOperationException("Expected camera interface failure was not reported"); }
        catch (T) { }
    }
    private static async Task Until(Func<bool> predicate) {
        var clock = Stopwatch.StartNew();
        while (!predicate()) { if (clock.Elapsed > TimeSpan.FromSeconds(15)) throw new TimeoutException("Camera output completion expired"); await Task.Delay(5); }
    }
    internal static async Task Run(string executable, bool direct, bool standard = false, bool nested = false)
    {
        executable = Path.GetFullPath(executable);
        await HubCameraHostFixture.Run(executable, direct, standard, nested, async (firstBinding, secondBinding) => {
            using var first = new CameraOutput(firstBinding, executable);
            using var second = new CameraOutput(secondBinding, executable);
            Check(first is ICameraV2 && first is ICameraV3 && first is ICameraV4 && first.InterfaceVersion == 4, "Camera output compatibility interfaces missing");
            Check(!first.Connected && !first.Connecting && first.Name.Contains("SIMULATION"), "Camera metadata connected equipment or lost simulation label");
            first.Connected = true;
            second.Connect(); await Until(() => !second.Connecting);
            Check(first.Connected && second.Connected && first.CameraXSize > 0 && first.CameraYSize > 0, "Camera output connection failed");
            Failure<global::ASCOM.InvalidValueException>(() => first.NumX = 0);
            Failure<global::ASCOM.InvalidValueException>(() => first.StartExposure(double.NaN, true));
            Failure<global::ASCOM.InvalidValueException>(() => first.PulseGuide((GuideDirections)4, 1));
            first.NumX = 64; first.NumY = 64;
            Check(second.NumX == 64 && second.NumY == 64, "Camera output settings not shared");
            if (first.CanSetCCDTemperature) { first.SetCCDTemperature = -12; Check(second.SetCCDTemperature == -12, "Camera cooler setpoint lost"); }
            first.StartExposure(1.0, true);
            Failure<global::ASCOM.ValueNotSetException>(() => { _ = first.ImageArray; });
            Failure<global::ASCOM.DriverException>(() => second.AbortExposure());
            await Until(() => second.ImageReady);
            Check(Math.Abs(second.LastExposureDuration - 1.0) < 1e-6 && second.LastExposureStartTime.Contains("T"), "Camera exposure metadata changed");
            var typed = (Array)first.ImageArray; var variant = (Array)second.ImageArrayVariant;
            Check(typed.Rank == 2 && typed.GetLength(0) == 64 && typed.GetLength(1) == 64 && typed.GetType().GetElementType() == typeof(int), "ASCOM camera image shape/type changed");
            Check(variant.Rank == 2 && variant.GetType().GetElementType() == typeof(object) && variant.GetValue(3, 7) is int boxed && boxed == (int)typed.GetValue(3, 7)!, "ASCOM variant image changed pixel type/value");
            var pixel = typed.GetValue(3, 7);
            foreach (IStateValue state in first.DeviceState) {
                if (state.Name == "CameraState") Check(state.Value is CameraStates, "CameraState DeviceState enum lost");
                else if (state.Name == "PercentCompleted") Check(state.Value is short, "Camera PercentCompleted DeviceState lost Int16");
                else if (state.Name is "ImageReady" or "IsPulseGuiding") Check(state.Value is bool, "Camera boolean DeviceState lost type");
                else Check(state.Value is double, "Camera numeric DeviceState lost Double");
            }
            if (standard) {
                first.ReadoutMode = 1; first.StartExposure(0.05, false); await Until(() => first.ImageReady);
                var rgb = (Array)first.ImageArray;
                Check(rgb.Rank == 3 && rgb.GetLength(2) == 3 && rgb.GetType().GetElementType() == typeof(int), "ASCOM RGB rank/type changed");
                first.ReadoutMode = 2; first.StartExposure(0.05, false); await Until(() => first.ImageReady);
                var one = (Array)first.ImageArrayVariant;
                Check(one.Rank == 3 && one.GetLength(2) == 1, "One-plane ASCOM rank-three image collapsed");
                first.ReadoutMode = 0;
                Check(first.CanPulseGuide, "Camera guide capability lost");
                first.PulseGuide(GuideDirections.guideEast, 1000);
                Check(second.IsPulseGuiding, "Camera guide observation not shared");
            } else if (!first.CanPulseGuide) Failure<global::ASCOM.MethodNotImplementedException>(() => first.PulseGuide(GuideDirections.guideNorth, 0));
            first.StartExposure(0.1, false);
            first.Connected = false;
            Check(second.Connected, "Camera output disconnect revoked sibling");
            await Until(() => second.ImageReady);
            Check(Equals(typed.GetValue(3, 7), pixel), "Next capture/client disconnect mutated returned ASCOM array");
            if (standard) await Until(() => !second.IsPulseGuiding);
            second.Disconnect(); await Until(() => !second.Connecting);
            Console.WriteLine($"Native ASCOM camera {IntPtr.Size * 8}-bit: interfaces, typed settings/images, shared acquisition and client loss passed");
        });
    }
}
