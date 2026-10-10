# Private, bounded Alpaca fixture for cross-kernel network acceptance. No devices.
use strict;
use warnings;
use IO::Socket::IP;
use IO::Select;

my ($family, $interface, $directory) = @ARGV;
die "usage: fixture ipv4|ipv6 INTERFACE DIRECTORY\n" unless defined $directory && -d $directory
    && $family =~ /^ipv[46]$/ && $interface =~ /^[a-zA-Z0-9_.-]+$/;
# Discover the address inside this long-lived process: a stopped WSL distro can
# receive a new MAC/link-local address when the next command starts it.
open my $ip, '-|', 'ip', '-o', ($family eq 'ipv4' ? '-4' : '-6'), 'address', 'show', 'dev', $interface or die $!;
my $interfaces = do { local $/; <$ip> };
close $ip or die "ip failed\n";
my ($address) = $family eq 'ipv4' ? $interfaces =~ /inet ([0-9.]+)\// : $interfaces =~ /inet6 (fe80:[0-9a-f:]+)\//i;
die "interface address unavailable\n" unless defined $address;
my $bind = $family eq 'ipv4' ? $address : "$address%$interface";
my $server = IO::Socket::IP->new(LocalHost => $bind, LocalService => 0,
    Listen => 8, ReuseAddr => 1) or die "listen: $!\n";
my $selector = IO::Select->new($server);
my %connected = (camera => 0, safetymonitor => 0);
my ($starts, $images, $transaction) = (0, 0, 0);
open my $routes_file, '<', "$directory/routes.tsv" or die $!;
my %routes;
while (<$routes_file>) { chomp; my ($key, $value) = split /\t/, $_, 2; $routes{$key} = $value; }
close $routes_file;
open my $image_file, '<:raw', "$directory/image.bin" or die $!;
my $image = do { local $/; <$image_file> };
close $image_file;
open my $traffic, '>', "$directory/upstream.tsv" or die $!;
$traffic->autoflush(1);
open my $ready, '>', "$directory/ready.tmp" or die $!;
print $ready $server->sockport, "\n", $address, "\n", $$, "\n";
close $ready;
rename "$directory/ready.tmp", "$directory/ready" or die $!;
# The parent requests normal termination through this private directory. A hard
# bound also retires the fixture if its parent is lost; no distro-wide shutdown.
$SIG{ALRM} = sub { die "fixture lifetime exceeded\n" };
alarm 180;
my $expires = time + 180;
while (!-e "$directory/stop") {
    next unless $selector->can_read(0.1);
    my $client = $server->accept() or die "accept: $!\n";
    $client->autoflush(1);
    my $peer = $client->peerhost;
    eval {
        local $SIG{ALRM} = sub { die "request deadline\n" };
        alarm 5;
        my $head = '';
        while ($head !~ /\r\n\r\n\z/) {
            my $byte;
            die "request ended\n" unless read($client, $byte, 1);
            $head .= $byte;
            die "headers too large\n" if length($head) > 16384;
        }
        my ($method, $target) = $head =~ /^(GET|PUT) (\S+) HTTP\/1\.[01]\r\n/;
        die "invalid request\n" unless defined $method;
        my ($length) = $head =~ /\r\nContent-Length:\s*(\d+)/i;
        $length //= 0;
        die "body too large\n" if $length > 4096;
        my $body = '';
        while (length($body) < $length) {
            my $part;
            die "body ended\n" unless read($client, $part, $length - length($body));
            $body .= $part;
        }
        my ($path, $query) = split /\?/, $target, 2;
        my $parameters = ($query // '') . '&' . $body;
        my ($client_transaction) = $parameters =~ /(?:^|&)ClientTransactionID=(\d+)(?:&|$)/i;
        $client_transaction //= 0;
        ++$transaction;
        my ($kind, $member) = $path =~ m{^/api/v1/(camera|safetymonitor)/0/([a-z]+)$};
        my ($value, $error) = ('null', 0);
        if (defined $member && $member eq 'connected') {
            if ($method eq 'PUT') {
                my ($setting) = $parameters =~ /(?:^|&)Connected=(true|false)(?:&|$)/i;
                die "invalid connection setting\n" unless defined $setting;
                $connected{$kind} = lc($setting) eq 'true' ? 1 : 0;
            }
            $value = $connected{$kind} ? 'true' : 'false';
        } elsif (defined $kind && !$connected{$kind} && $member ne 'interfaceversion') {
            $error = 1031;
        } elsif (defined $member && $member eq 'startexposure' && $method eq 'PUT') {
            ++$starts;
        } elsif (defined $member && $member eq 'imageready') {
            $value = $starts ? 'true' : 'false';
        } elsif ($method eq 'GET' && exists $routes{$path}) {
            $value = $routes{$path};
        } else { $error = 1024; }
        my $type = 'application/json';
        my $response = '{"ClientTransactionID":' . $client_transaction . ',"ServerTransactionID":' . $transaction
            . ',"ErrorNumber":' . $error . ',"ErrorMessage":"","Value":' . $value . '}';
        if (defined $member && $member eq 'imagearray' && !$error) {
            ++$images;
            if ($head =~ /\r\nAccept:[^\r\n]*application\/imagebytes/i) {
                $response = $image;
                substr($response, 8, 8, pack('V2', $client_transaction, $transaction));
                $type = 'application/imagebytes';
            }
        }
        print $traffic join("\t", $peer, $method, $path, $error, $type), "\n";
        print $client "HTTP/1.1 200 OK\r\nContent-Type: $type\r\nContent-Length: " . length($response)
            . "\r\nConnection: close\r\n\r\n", $response or die "response: $!\n";
        open my $state, '>', "$directory/state.json" or die $!;
        print $state '{"cameraConnected":' . ($connected{camera} ? 'true' : 'false')
            . ',"safetyConnected":' . ($connected{safetymonitor} ? 'true' : 'false')
            . ',"starts":' . $starts . ',"images":' . $images . '}';
        close $state;
    };
    my $error = $@;
    close $client;
    die $error if $error;
    my $remaining = $expires - time;
    die "fixture lifetime exceeded\n" if $remaining <= 0;
    alarm $remaining;
}
close $traffic;
close $server;
