using ZwoGain.Rotator;

namespace ZwoGain.NINA;

internal static class CaaSettings
{
    public static void Show(CaaSession session, bool ownedByNina) => CaaSetupWindow.Show(session, ownedByNina);
}
