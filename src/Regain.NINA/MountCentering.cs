using System.ComponentModel.Composition;
using NINA.Astrometry;
using NINA.Core.Model;
using NINA.Core.Utility.WindowService;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.Mediator;
using NINA.PlateSolving;
using NINA.PlateSolving.Interfaces;
using NINA.Profile.Interfaces;
using NINA.Sequencer.SequenceItem.Platesolving;
using NINA.WPF.Base.Interfaces.Mediator;

namespace Regain.NINA;

/// <summary>Reuse NINA's solver/configuration without Center.Execute's automatic guiding restart.</summary>
[Export(typeof(MountCentering))]
[PartCreationPolicy(CreationPolicy.NonShared)]
public class MountCentering : Center
{
    [ImportingConstructor]
    public MountCentering(IProfileService profiles, ITelescopeMediator telescope, IImagingMediator imaging,
        IFilterWheelMediator wheel, IGuiderMediator guider, IDomeMediator dome, IDomeFollower follower,
        IPlateSolverFactory solvers, IWindowServiceFactory windows)
        : base(profiles, telescope, imaging, wheel, guider, dome, follower, solvers, windows) { }
    internal async Task CenterVerifiedAsync(Coordinates target, IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        Inherited = false; Coordinates = new InputCoordinates(MountRecoveryService.Copy(target));
        var result = await DoCenter(progress, token).ConfigureAwait(false);
        token.ThrowIfCancellationRequested();
        if (!result.Success) throw new IOException("Recovery plate solve/centering failed; guiding remains stopped.");
    }
}
