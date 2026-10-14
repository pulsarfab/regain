using System.ComponentModel.Composition;
using System.Windows;

namespace Regain.NINA;

[Export(typeof(ResourceDictionary))]
public partial class MountRecoveryTemplates : ResourceDictionary
{
    public MountRecoveryTemplates() { InitializeComponent(); }
    private void Reconcile(object sender, RoutedEventArgs args)
    {
        var context = (sender as FrameworkElement)?.DataContext;
        if (MessageBox.Show("Verify the mount has stopped, inspect its position and the previous recovery outcome, and confirm the selected driver and target. Allow another recovery?",
            "Regain mount recovery", MessageBoxButton.YesNo, MessageBoxImage.Question) != MessageBoxResult.Yes) return;
        bool allowed = context switch { RecoverMount step => step.AllowAfterInspection(), RecoverMountOnTrackingLoss trigger => trigger.AllowAfterInspection(), _ => false };
        if (!allowed) MessageBox.Show("The previous mount operation is still in flight, its original mount is no longer selected, or its recovery journal cannot be cleared. Wait for completion and reselect that mount before clearing the fence.", "Regain mount recovery");
    }
}
