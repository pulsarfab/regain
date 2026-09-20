using System.IO.Compression;
using System.Security.Cryptography;
using Newtonsoft.Json;
using Newtonsoft.Json.Converters;
using NINA.Plugin.ManifestDefinition;
using NJsonSchema;
using Regain.NINA;

if (args.Length != 2) throw new ArgumentException("Usage: Regain.Packaging <archive.zip> <manifest.json>");
var archivePath = Path.GetFullPath(args[0]);
var plugin = new RegainPlugin();
string version = typeof(RegainPlugin).Assembly.GetName().Version!.ToString();
string archiveName = $"Regain-{version}.zip";
if (Path.GetFileName(archivePath) != archiveName) throw new InvalidDataException("Archive version does not match plugin assembly");
if (plugin.License != "Apache-2.0") throw new InvalidDataException("Plugin license must be Apache-2.0");
using (var archive = ZipFile.OpenRead(archivePath))
{
    string[] required = ["Regain.NINA.dll", "Regain.Core.dll", "Regain.Rotator.dll", "regain-caa.exe", "regain-accessories.exe", "regain-ofp2.exe", "regain-fc3.exe", "caa.md", "regain-host.exe", "regain-direct.exe", "regain-camera.exe", "regain-alpaca.exe", "ASICamera2.dll",
        "LICENSE", "THIRD_PARTY_NOTICES.md", "regain.png", "licenses/ZWO-ASI-SDK.txt", "licenses/Rust-Standard-Library.html"];
    foreach (string name in required)
        if (archive.GetEntry(name) is not { Length: > 0 }) throw new InvalidDataException($"Package missing {name}");
    foreach (var entry in archive.Entries)
    {
        if (entry.FullName.Split('/').Contains("..") || entry.FullName.StartsWith('/') || entry.FullName.Contains('\\'))
            throw new InvalidDataException($"Invalid archive path {entry.FullName}");
        if (Path.GetFileName(entry.FullName).StartsWith("NINA.", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Do not package the NINA application dependencies");
    }
    using var license = new StreamReader(archive.GetEntry("LICENSE")!.Open());
    if (!(await license.ReadToEndAsync()).Contains("Apache License")) throw new InvalidDataException("Missing Apache license text");
    // Check the assemblies inside the ZIP against the versions we actually built.
    foreach (string name in new[] { "Regain.NINA.dll", "Regain.Core.dll", "Regain.Rotator.dll" })
    {
        using var stream = archive.GetEntry(name)!.Open();
        using var copy = new MemoryStream();
        await stream.CopyToAsync(copy);
        copy.Position = 0;
        using var pe = new System.Reflection.PortableExecutable.PEReader(copy);
        var metadata = System.Reflection.Metadata.PEReaderExtensions.GetMetadataReader(pe);
        if (metadata.GetAssemblyDefinition().Version.ToString() != version)
            throw new InvalidDataException($"Packaged {name} has a different version");
    }
}
string baseUrl = $"{plugin.Repository}/releases/download/v{version}";
using var file = File.OpenRead(archivePath);
string checksum = Convert.ToHexString(await SHA256.HashDataAsync(file));
var manifest = new PluginManifest
{
    Name = plugin.Name, Identifier = plugin.Identifier, Version = plugin.Version, Author = plugin.Author,
    Homepage = plugin.Homepage, Repository = plugin.Repository, License = plugin.License,
    LicenseURL = $"{plugin.Repository}/blob/v{version}/LICENSE",
    ChangelogURL = $"{plugin.Repository}/releases/tag/v{version}", Tags = plugin.Tags,
    MinimumApplicationVersion = plugin.MinimumApplicationVersion,
    Descriptions = new PluginDescription
    {
        ShortDescription = plugin.Descriptions.ShortDescription, LongDescription = plugin.Descriptions.LongDescription,
        FeaturedImageURL = $"{baseUrl}/regain.png", ScreenshotURL = "", AltScreenshotURL = ""
    },
    Installer = new PluginInstallerDetails
    {
        URL = $"{baseUrl}/{archiveName}", Type = InstallerType.ARCHIVE,
        Checksum = checksum, ChecksumType = InstallerChecksum.SHA256
    }
};
string json = JsonConvert.SerializeObject(manifest, Formatting.Indented, new StringEnumConverter());
var schema = await JsonSchema.FromJsonAsync(PluginManifest.Schema);
var errors = schema.Validate(json);
if (errors.Count != 0) throw new InvalidDataException(string.Join(Environment.NewLine, errors));
await File.WriteAllTextAsync(args[1], json + Environment.NewLine);
Console.WriteLine($"Validated NINA manifest: {args[1]}");
