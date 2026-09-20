using Regain.Rotator;

namespace Regain.NINA;

internal static class CaaSettings
{
    public static void Show(CaaSession session, bool ownedByNina) => CaaSetupWindow.Show(session, ownedByNina);
}
