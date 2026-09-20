using System.Windows;

namespace Regain.Rotator;

public static class CaaSetupForm
{
    public static void ShowModal(CaaSession session)
    {
        if (Application.Current is not null) {
            CaaSetupWindow.Show(session, ownedByApplication: session.Connected);
            return;
        }
        Exception? error = null;
        var thread = new Thread(() => {
            try { CaaSetupWindow.Show(session, ownedByApplication: session.Connected); }
            catch (Exception e) { error = e; }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start(); thread.Join();
        if (error is not null) throw error;
    }
}
