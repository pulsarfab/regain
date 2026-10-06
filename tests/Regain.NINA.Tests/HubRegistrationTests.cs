using Microsoft.Win32;
using Regain.Hub;
using Regain.Hub.ASCOM;
using Xunit;

public sealed class HubRegistrationTests
{
    internal sealed class Fixture : IDisposable
    {
        internal readonly string Directory = Path.Combine(Path.GetTempPath(), "regain registration " + Guid.NewGuid().ToString("N"));
        private readonly string key = @"Software\PulsarFab\Regain\Tests\" + Guid.NewGuid().ToString("N");
        internal readonly RegistryKey Parent;
        internal readonly RegistryKey[] Roots;
        internal readonly HubSelectionStore Store;
        internal HubSelection Binding;
        internal HubSelections Saved;
        internal string Class => @"Software\Classes\CLSID\" + OutputIdentity.ClassId(Binding).ToString("B");
        internal Fixture()
        {
            System.IO.Directory.CreateDirectory(Path.Combine(Directory, "hub-ascom", "x64"));
            File.WriteAllText(Path.Combine(Directory, "hub-ascom", "x64", "Regain.Hub.ASCOM.exe"), "fixture, never executed");
            File.WriteAllText(Path.Combine(Directory, "regain-alpaca.exe"), "fixture, never executed");
            Parent = Registry.CurrentUser.CreateSubKey(key);
            Roots = [Parent.CreateSubKey("view32"), Parent.CreateSubKey("view64")];
            Store = new HubSelectionStore(Path.Combine(Directory, "selections.json"));
            Binding = new HubSelection { ConfigPath = Path.Combine(Directory, "hub.json"), InstanceId = Guid.NewGuid(),
                OutputId = Guid.NewGuid(), DeviceType = "switch", Label = "Observatory controls", Simulated = true };
            Saved = Store.Save(Binding, Guid.Empty);
        }
        internal Guid Register(Guid? revision = null) => HubAscomRegistration.RegisterSaved(Roots, Directory, Store.Path,
            revision ?? Saved.Revision, Binding.InstanceId, Binding.OutputId, HubAscomRegistration.CurrentOwner);
        internal void Remove() => HubAscomRegistration.Remove(Roots, Directory, OutputIdentity.ClassId(Binding), HubAscomRegistration.CurrentOwner);
        internal object? Read(int view, string path, string value = "") { using var entry = Roots[view].OpenSubKey(path); return entry?.GetValue(value); }
        internal void Write(int view, string path, string value, object content) { using var entry = Roots[view].CreateSubKey(path); entry.SetValue(value, content); }
        public void Dispose()
        {
            foreach (var root in Roots) root.Dispose();
            Parent.Dispose(); Registry.CurrentUser.DeleteSubKeyTree(key, false);
            System.IO.Directory.Delete(Directory, true);
        }
    }

    [Fact]
    public void ActualRegistryValuesBindBothViewsToOneOwnerAndIdentityWithoutChangingSelections()
    {
        using var f = new Fixture();
        var id = f.Register();
        Assert.Equal(OutputIdentity.ClassId(f.Binding), id);
        Assert.Equal(f.Saved.Revision, f.Store.Load().Revision);
        foreach (var index in new[] { 0, 1 }) {
            Assert.Equal([id], HubAscomRegistration.RegisteredIds(f.Roots[index], f.Directory));
            var command = Assert.IsType<string>(f.Read(index, f.Class + @"\LocalServer32"));
            Assert.Contains("--owner-sid " + HubAscomRegistration.CurrentOwner, command);
            Assert.Contains("--bindings \"" + f.Store.Path + "\"", command);
            Assert.Contains("hub-ascom\\x64\\Regain.Hub.ASCOM.exe", command);
            Assert.Equal(id.ToString("B"), f.Read(index, f.Class, "AppID"));
            Assert.Equal("Interactive User", f.Read(index, @"Software\Classes\AppID\" + id.ToString("B"), "RunAs"));
            Assert.Contains("SIMULATION", Assert.IsType<string>(f.Read(index, @"Software\ASCOM\Switch Drivers\" + OutputIdentity.ProgId(f.Binding))));
        }
        f.Remove(); f.Remove(); // Explicit removal is idempotent after exact cleanup.
        Assert.All(f.Roots, root => Assert.Empty(HubAscomRegistration.RegisteredIds(root)));
    }

    [Fact]
    public void RenameRequiresCurrentSelectionRevisionAndKeepsTheSameClsid()
    {
        using var f = new Fixture();
        var id = f.Register(); var old = f.Saved.Revision;
        f.Binding.Label = "Renamed controls";
        f.Saved = f.Store.Save(f.Binding, old);
        Assert.Throws<InvalidOperationException>(() => f.Register(old));
        Assert.DoesNotContain("Renamed", Assert.IsType<string>(f.Read(0, f.Class)));
        Assert.Equal(id, f.Register());
        Assert.Contains("Renamed controls", Assert.IsType<string>(f.Read(1, f.Class)));
    }

    [Fact]
    public void CollisionInSecondViewPreventsEveryFirstViewMutation()
    {
        using var f = new Fixture();
        f.Write(1, f.Class + @"\LocalServer32", "", "foreign command");
        Assert.Throws<InvalidOperationException>(() => f.Register());
        Assert.Null(f.Read(0, f.Class));
        Assert.Empty(HubAscomRegistration.RegisteredIds(f.Roots[0]));
        Assert.Equal("foreign command", f.Read(1, f.Class + @"\LocalServer32"));
    }

    [Fact]
    public void ChangedCommandOrNewerInventoryCannotBeOverwrittenOrRemoved()
    {
        using var f = new Fixture(); f.Register();
        var path = f.Class + @"\LocalServer32";
        var original = f.Read(1, path)!;
        f.Write(1, path, "", "foreign command");
        Assert.Throws<InvalidOperationException>(() => f.Register());
        Assert.Throws<InvalidOperationException>(f.Remove);
        Assert.NotNull(f.Read(0, f.Class));
        f.Write(1, path, "", original);
        f.Write(1, HubAscomRegistration.Inventory + "\\" + OutputIdentity.ClassId(f.Binding).ToString("B"), "Version", "99.0.0.0");
        Assert.Throws<InvalidOperationException>(f.Remove);
        Assert.Throws<InvalidOperationException>(() => f.Register());
        Assert.NotNull(f.Read(0, f.Class));
    }

    [Fact]
    public void RemovalDoesNotNeedSelectionsOrExecutablesAndPreservesOtherInstalls()
    {
        using var f = new Fixture(); var id = f.Register();
        var other = Path.Combine(f.Directory, "another-install");
        System.IO.Directory.CreateDirectory(Path.Combine(other, "hub-ascom", "x64"));
        File.WriteAllText(Path.Combine(other, "hub-ascom", "x64", "Regain.Hub.ASCOM.exe"), "fixture");
        File.WriteAllText(Path.Combine(other, "regain-alpaca.exe"), "fixture");
        var second = f.Binding.Copy(); second.InstanceId = Guid.NewGuid(); second.OutputId = Guid.NewGuid();
        f.Saved = f.Store.Save(second, f.Saved.Revision);
        var otherId = HubAscomRegistration.RegisterSaved(f.Roots, other, f.Store.Path, f.Saved.Revision,
            second.InstanceId, second.OutputId, HubAscomRegistration.CurrentOwner);
        Assert.Equal([id], HubAscomRegistration.RegisteredIds(f.Roots[0], f.Directory));
        File.Delete(f.Store.Path); File.Delete(Path.Combine(f.Directory, "regain-alpaca.exe"));
        f.Remove();
        Assert.Equal([otherId], HubAscomRegistration.RegisteredIds(f.Roots[0]));
        Assert.Throws<InvalidOperationException>(() => HubAscomRegistration.Remove(f.Roots, f.Directory, otherId, null));
        Assert.Equal([otherId], HubAscomRegistration.RegisteredIds(f.Roots[1]));
    }

    [Fact]
    public void PartialOwnedRegistrationCanBeExplicitlyRepairedAndForeignOwnerIsRejected()
    {
        using var f = new Fixture(); var id = f.Register();
        var inventory = HubAscomRegistration.Inventory + "\\" + id.ToString("B");
        f.Write(0, inventory, "Phase", "pending");
        f.Roots[0].DeleteSubKeyTree(f.Class, false);
        Assert.Equal(id, f.Register());
        Assert.Equal("ready", f.Read(0, inventory, "Phase"));
        f.Write(1, inventory, "OwnerSid", "S-1-5-18");
        Assert.Throws<InvalidOperationException>(() => f.Register());
        Assert.Throws<InvalidOperationException>(f.Remove);
        Assert.Equal("ready", f.Read(0, inventory, "Phase"));
    }

    [Fact]
    public void MutationFailureRestoresFirstViewAndRetainsUnrelatedRegistryValues()
    {
        using var f = new Fixture();
        using var readOnly = f.Parent.OpenSubKey("view64", false)!;
        Assert.Throws<InvalidOperationException>(() => HubAscomRegistration.RegisterSaved([f.Roots[0], readOnly], f.Directory,
            f.Store.Path, f.Saved.Revision, f.Binding.InstanceId, f.Binding.OutputId, HubAscomRegistration.CurrentOwner));
        Assert.Empty(HubAscomRegistration.RegisteredIds(f.Roots[0]));
        Assert.Null(f.Read(0, f.Class));
        f.Register();
        using var beforeKey = f.Roots[0].OpenSubKey(f.Class, true)!;
        var security = HubAscomRegistration.ReadSecurity(beforeKey);
        security[3] |= 0x10; // Protect the DACL from inherited changes.
        HubAscomRegistration.RestoreSecurity(beforeKey, security);
        security = HubAscomRegistration.ReadSecurity(beforeKey);
        beforeKey.Dispose();
        f.Write(0, f.Class, "extra", new byte[] { 1, 2, 3 });
        using var readOnlyExisting = f.Parent.OpenSubKey("view64", false)!;
        f.Binding.Label = "Updated"; f.Saved = f.Store.Save(f.Binding, f.Saved.Revision);
        Assert.Throws<InvalidOperationException>(() => HubAscomRegistration.RegisterSaved([f.Roots[0], readOnlyExisting], f.Directory,
            f.Store.Path, f.Saved.Revision, f.Binding.InstanceId, f.Binding.OutputId, HubAscomRegistration.CurrentOwner));
        Assert.DoesNotContain("Updated", Assert.IsType<string>(f.Read(0, f.Class)));
        Assert.Equal(new byte[] { 1, 2, 3 }, f.Read(0, f.Class, "extra"));
        using var afterKey = f.Roots[0].OpenSubKey(f.Class)!;
        Assert.Equal(security, HubAscomRegistration.ReadSecurity(afterKey));
    }

    [Fact]
    public void OversizedOrDeepOwnedTreesAreRejectedBeforeAnyRegistryMutation()
    {
        using var f = new Fixture(); f.Register();
        f.Binding.Label = "Updated"; f.Saved = f.Store.Save(f.Binding, f.Saved.Revision);
        f.Write(1, f.Class, "oversized", new byte[1024 * 1024]);
        Assert.Throws<InvalidOperationException>(() => f.Register());
        Assert.DoesNotContain("Updated", Assert.IsType<string>(f.Read(0, f.Class)));
        using (var key = f.Roots[1].OpenSubKey(f.Class, true)!) key.DeleteValue("oversized");
        var deep = f.Class + "\\" + string.Join("\\", Enumerable.Repeat("nested", 17));
        f.Write(1, deep, "", "fixture");
        Assert.Throws<InvalidOperationException>(() => f.Register());
        Assert.Throws<InvalidOperationException>(f.Remove);
        Assert.DoesNotContain("Updated", Assert.IsType<string>(f.Read(0, f.Class)));
        Assert.Equal("fixture", f.Read(1, deep));
    }

    [Fact]
    public void RegistrationLockRejectsConcurrentSelectionChangesAndNeverConnectsAHost()
    {
        using var f = new Fixture();
        f.Store.WithRevision(f.Saved.Revision, saved => {
            Assert.Throws<InvalidOperationException>(() => new HubSelectionStore(f.Store.Path).Remove(f.Binding.InstanceId, f.Binding.OutputId, saved.Revision));
            return 0;
        });
        Assert.Equal(f.Saved.Revision, f.Store.Load().Revision);
        f.Register(); // The deliberately non-executable fixture files are never launched.
        Assert.Throws<InvalidOperationException>(() => HubAscomRegistration.RegisterSaved(f.Roots, f.Directory, f.Store.Path,
            f.Saved.Revision, f.Binding.InstanceId, f.Binding.OutputId, "S-1-5-18"));
    }
}
