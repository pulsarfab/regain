using System.Runtime.InteropServices;
using System.Text.Json;
using ASCOM.DeviceInterface;

namespace Regain.Hub.ASCOM;

[ComVisible(true), Guid("5b21fbfa-c238-4f3a-8aca-2f6f7f68da32"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(IRotatorV4))]
public sealed class RotatorOutput : OutputDriver, IRotatorV4, IRotatorV3, IRotatorV2
{
    public RotatorOutput(HubSelection binding, string executable, string? workers = null) : base(SwitchOutput.Require(binding, "rotator"), executable, workers) { }
    public override short InterfaceVersion => 4;
    private JsonElement Read(HubRotatorProperty property)
    {
        var value = Get(HubRotatorProtocol.Read(property), property.ToString());
        try { return HubRotatorProtocol.Validate(property, value); }
        catch (HubException) { throw new global::ASCOM.DriverException("Hub returned an invalid rotator reading; inspect source health"); }
    }
    public bool CanReverse => Read(HubRotatorProperty.CanReverse).GetBoolean();
    public bool IsMoving => Read(HubRotatorProperty.IsMoving).GetBoolean();
    public float MechanicalPosition => Read(HubRotatorProperty.MechanicalPosition).GetSingle();
    public float Position => Read(HubRotatorProperty.Position).GetSingle();
    public float TargetPosition => Read(HubRotatorProperty.TargetPosition).GetSingle();
    public float StepSize => Read(HubRotatorProperty.StepSize).GetSingle();
    public bool Reverse { get => Read(HubRotatorProperty.Reverse).GetBoolean(); set => Put(HubRotatorProtocol.Reverse(value), "Reverse", false); }
    private static float Angle(float position, bool absolute)
    {
        try { HubRotatorProtocol.ValidateCommand(position, absolute); return position; }
        catch (ArgumentOutOfRangeException) {
            throw new global::ASCOM.InvalidValueException("Position", "invalid", absolute ? "0 <= angle < 360" : "finite signed angle");
        }
    }
    public void Move(float position) => Put(HubRotatorProtocol.Move(Angle(position, false)), "Move");
    public void MoveAbsolute(float position) => Put(HubRotatorProtocol.MoveAbsolute(Angle(position, true)), "MoveAbsolute");
    public void MoveMechanical(float position) => Put(HubRotatorProtocol.MoveMechanical(Angle(position, true)), "MoveMechanical");
    public void Sync(float position) => Put(HubRotatorProtocol.Sync(Angle(position, true)), "Sync");
    public void Halt() => Put(HubRotatorProtocol.Halt(), "Halt");
}
