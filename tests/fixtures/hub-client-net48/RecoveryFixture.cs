using System.Text.Json;
using System.Windows.Automation;
using System.Windows.Controls;
using Regain.Rotator;

internal static class RecoveryFixture
{
    public static void Run()
    {
        Exception? error=null;
        var thread=new Thread(()=> {
            try {
                var original=JsonDocument.Parse("{\"usbPortCycle\":true,\"legacyExtension\":\"keep\"}").RootElement;
                var form=new CameraRecoveryForm(original,_=>new StackPanel());
                if(form.Editors.Count!=17 || form.LiveEditors.Count!=2) throw new InvalidOperationException("Wrong Windows recovery field set");
                var logging=form.Editors.OfType<ComboBox>().Single(e=>AutomationProperties.GetAutomationId(e)=="recovery-transferLogMode");
                var power=form.Editors.OfType<TextBox>().Single(e=>AutomationProperties.GetAutomationId(e)=="recovery-coolerMaxPowerChangePer5Seconds");
                if((string)logging.SelectedItem!="circular" || double.Parse(power.Text, System.Globalization.CultureInfo.InvariantCulture)!=25 || !form.LiveEditors.Contains(logging) || !form.LiveEditors.Contains(power))
                    throw new InvalidOperationException("Live camera defaults or metadata changed");
                logging.SelectedItem="off"; power.Text="5";
                var reconnect=form.Editors.OfType<TextBox>().Single(e=>AutomationProperties.GetAutomationId(e)=="recovery-reconnectDelaySeconds");
                reconnect.Text="0.000001";
                var changed=form.Read();
                if(changed.GetProperty("reconnectDelaySeconds").GetDouble()!=.000001 ||
                   !changed.GetProperty("usbPortCycle").GetBoolean() ||
                   changed.GetProperty("legacyExtension").GetString()!="keep" ||
                   changed.GetProperty("maxRetries").GetInt32()!=3 ||
                   changed.GetProperty("preExposureReconnectAttempts").GetInt32()!=3 ||
                   changed.GetProperty("transferLogMode").GetString()!="off" ||
                   changed.GetProperty("coolerMaxPowerChangePer5Seconds").GetDouble()!=5)
                    throw new InvalidOperationException("Recovery defaults or hidden values changed");
                reconnect.Text="0";
                try {form.Read();throw new InvalidOperationException("Zero positive timeout was accepted");}
                catch(ArgumentException) { }
                Console.WriteLine("net48 recovery metadata, strict positive limits, sparse defaults and hidden values pass.");
            } catch(Exception e) {error=e;}
        });
        thread.SetApartmentState(ApartmentState.STA);thread.Start();thread.Join();
        if(error is not null) throw error;
    }
}
