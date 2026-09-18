using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

namespace ZwoGain.Rotator;

public static class SetupTheme
{
    public static void Apply(Window window)
    {
        // Use the host's theme in NINA; standalone ASCOM clients get a light theme.
        if (Application.Current?.TryFindResource("BackgroundBrush") is null) {
            window.Resources["BackgroundBrush"] = new SolidColorBrush(Color.FromRgb(245, 247, 248));
            window.Resources["PrimaryBrush"] = new SolidColorBrush(Color.FromRgb(32, 47, 53));
            window.FontFamily = new FontFamily("Segoe UI"); window.FontSize = 14;
            var buttons = new Style(typeof(Button));
            buttons.Setters.Add(new Setter(Control.BackgroundProperty, new SolidColorBrush(Color.FromRgb(229, 237, 239))));
            buttons.Setters.Add(new Setter(Control.BorderThicknessProperty, new Thickness(0)));
            var border = new FrameworkElementFactory(typeof(Border));
            border.SetBinding(Border.BackgroundProperty, new System.Windows.Data.Binding("Background") { RelativeSource = System.Windows.Data.RelativeSource.TemplatedParent });
            border.SetBinding(Border.PaddingProperty, new System.Windows.Data.Binding("Padding") { RelativeSource = System.Windows.Data.RelativeSource.TemplatedParent });
            border.SetValue(Border.CornerRadiusProperty, new CornerRadius(3));
            var content = new FrameworkElementFactory(typeof(ContentPresenter));
            content.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
            content.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            border.AppendChild(content);
            buttons.Setters.Add(new Setter(Control.TemplateProperty, new ControlTemplate(typeof(Button)) { VisualTree = border }));
            var disabled = new Trigger { Property = UIElement.IsEnabledProperty, Value = false };
            disabled.Setters.Add(new Setter(UIElement.OpacityProperty, .45)); buttons.Triggers.Add(disabled);
            var hover = new Trigger { Property = UIElement.IsMouseOverProperty, Value = true };
            hover.Setters.Add(new Setter(Control.BackgroundProperty, new SolidColorBrush(Color.FromRgb(204, 224, 226)))); buttons.Triggers.Add(hover);
            window.Resources[typeof(Button)] = buttons;
            var tabStyle = new Style(typeof(TabItem)); tabStyle.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(10, 6, 10, 6)));
            window.Resources[typeof(TabItem)] = tabStyle;
        }
        window.SetResourceReference(Window.BackgroundProperty, "BackgroundBrush");
        window.SetResourceReference(Window.ForegroundProperty, "PrimaryBrush");
    }
}
