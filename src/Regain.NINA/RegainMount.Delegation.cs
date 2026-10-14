using NINA.Astrometry;
using NINA.Core.Enum;
using NINA.Equipment.Interfaces;
namespace Regain.NINA;
public sealed partial class RegainMount
{
    public string Action(string actionName, string actionParameters) => upstream.Action(actionName, actionParameters);
    public string SendCommandString(string command, bool raw = true) => upstream.SendCommandString(command, raw);
    public bool SendCommandBool(string command, bool raw = true) => upstream.SendCommandBool(command, raw);
    public void SendCommandBlind(string command, bool raw = true) => upstream.SendCommandBlind(command, raw);
    public Coordinates Coordinates => upstream.Coordinates;
    public double RightAscension => upstream.RightAscension;
    public string RightAscensionString => upstream.RightAscensionString;
    public double Declination => upstream.Declination;
    public string DeclinationString => upstream.DeclinationString;
    public double SiderealTime => upstream.SiderealTime;
    public string SiderealTimeString => upstream.SiderealTimeString;
    public double Altitude => upstream.Altitude;
    public string AltitudeString => upstream.AltitudeString;
    public double Azimuth => upstream.Azimuth;
    public string AzimuthString => upstream.AzimuthString;
    public double HoursToMeridian => upstream.HoursToMeridian;
    public string HoursToMeridianString => upstream.HoursToMeridianString;
    public double TimeToMeridianFlip => upstream.TimeToMeridianFlip;
    public string TimeToMeridianFlipString => upstream.TimeToMeridianFlipString;
    public double PrimaryMovingRate { get => upstream.PrimaryMovingRate; set => upstream.PrimaryMovingRate = value; }
    public double SecondaryMovingRate { get => upstream.SecondaryMovingRate; set => upstream.SecondaryMovingRate = value; }
    public PierSide SideOfPier => upstream.SideOfPier;
    public bool CanSetTrackingEnabled => upstream.CanSetTrackingEnabled;
    public IList<TrackingMode> TrackingModes => upstream.TrackingModes;
    public TrackingRate TrackingRate => upstream.TrackingRate;
    public double SiteLatitude { get => upstream.SiteLatitude; set => upstream.SiteLatitude = value; }
    public double SiteLongitude { get => upstream.SiteLongitude; set => upstream.SiteLongitude = value; }
    public double SiteElevation { get => upstream.SiteElevation; set => upstream.SiteElevation = value; }
    public bool AtHome => upstream.AtHome;
    public bool CanFindHome => upstream.CanFindHome;
    public bool AtPark => upstream.AtPark;
    public bool CanPark => upstream.CanPark;
    public bool CanUnpark => upstream.CanUnpark;
    public bool CanSetPark => upstream.CanSetPark;
    public Epoch EquatorialSystem => upstream.EquatorialSystem;
    public bool HasUnknownEpoch => upstream.HasUnknownEpoch;
    public Coordinates TargetCoordinates => upstream.TargetCoordinates;
    public PierSide? TargetSideOfPier => upstream.TargetSideOfPier;
    public bool Slewing => upstream.Slewing;
    public double GuideRateRightAscensionArcsecPerSec => upstream.GuideRateRightAscensionArcsecPerSec;
    public double GuideRateDeclinationArcsecPerSec => upstream.GuideRateDeclinationArcsecPerSec;
    public bool CanMovePrimaryAxis => upstream.CanMovePrimaryAxis;
    public bool CanMoveSecondaryAxis => upstream.CanMoveSecondaryAxis;
    public bool CanSetDeclinationRate => upstream.CanSetDeclinationRate;
    public bool CanSetRightAscensionRate => upstream.CanSetRightAscensionRate;
    public AlignmentMode AlignmentMode => upstream.AlignmentMode;
    public bool CanPulseGuide => upstream.CanPulseGuide;
    public bool IsPulseGuiding => upstream.IsPulseGuiding;
    public bool CanSetPierSide => upstream.CanSetPierSide;
    public bool CanSlew => upstream.CanSlew;
    public bool CanSlewAltAz => upstream.CanSlewAltAz;
    public DateTime UTCDate => upstream.UTCDate;
    public IList<(double, double)> GetAxisRates(TelescopeAxes axis) => upstream.GetAxisRates(axis);
    public void PulseGuide(GuideDirections direction, int duration) => upstream.PulseGuide(direction, duration);
    public void Setpark() => upstream.Setpark();
    public void StopSlew() => upstream.StopSlew();
    public PierSide DestinationSideOfPier(Coordinates coordinates) => upstream.DestinationSideOfPier(coordinates);
    private int manualAxes;
    public void MoveAxis(TelescopeAxes axis, double rate) {
        if (recoveryFailed && rate != 0) throw new IOException("Mount recovery failed; inspect before moving an axis.");
        int bit = 1 << (int)axis;
        if (rate != 0) Interlocked.Or(ref manualAxes, bit);
        upstream.MoveAxis(axis, rate);
        if (rate == 0) { Interlocked.And(ref manualAxes, ~bit); Health.ResetPositionBaseline(); }
    }
    public void SetCustomTrackingRate(double rightAscensionRate, double declinationRate) {
        if (recoveryFailed) throw new IOException("Mount recovery failed; inspect before setting custom tracking.");
        upstream.SetCustomTrackingRate(rightAscensionRate, declinationRate);
        if (!Recovering) { intentionalStop = false; Health.Intent(true); }
    }
}
