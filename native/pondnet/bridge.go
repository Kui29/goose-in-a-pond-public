package pondnet

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/internal/privatefile"
	"golang.org/x/net/netutil"
)

// PeerHeader and MachineHeader are accepted only by the Pond's private Unix listener. Never
// trust them on an HTTP/TLS listener accessible by another machine.
const (
	PeerHeader    = "X-Pond-Embedded-Peer"
	MachineHeader = "X-Pond-Embedded-Machine"
)

// MachineOf names the tailnet machine key at a peer address, so the Pond can refuse a phone's
// token arriving from any other machine.
type MachineOf func(ctx context.Context, remoteAddr string) (string, error)

type machineKey struct{}

// LoadCertificate reads the existing Pond identity without creating or changing
// a key. Reloading on each handshake preserves Rust's certificate-renewal policy.
func LoadCertificate(path string) (*tls.Certificate, error) {
	file, err := privatefile.Open(path, 65536)
	if errors.Is(err, privatefile.ErrNotPrivate) {
		return nil, errors.New("Pond TLS identity must be a private regular file")
	}
	if err != nil {
		return nil, err
	}
	defer file.Close()
	var material struct {
		Key  string `json:"key_pem"`
		Cert string `json:"cert_pem"`
	}
	decoder := json.NewDecoder(io.LimitReader(file, 65537))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&material); err != nil {
		return nil, err
	}
	if err := decoder.Decode(new(any)); err != io.EOF {
		return nil, errors.New("trailing TLS identity data")
	}
	certificate, err := tls.X509KeyPair([]byte(material.Cert), []byte(material.Key))
	return &certificate, err
}

// BridgeHandler forwards only companion API requests to a private Unix socket,
// overwriting all peer metadata from the authenticated transport connection.
func BridgeHandler(socket string, machineOf MachineOf) http.Handler {
	target := &url.URL{Scheme: "http", Host: "pond.internal"}
	// The Pond answers every route's headers promptly, event streams included, so a
	// socket that has not by then is stuck rather than slow.
	transport := &http.Transport{MaxIdleConns: 16, IdleConnTimeout: 30 * time.Second, ResponseHeaderTimeout: 30 * time.Second,
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			return (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, "unix", socket)
		}}
	proxy := &httputil.ReverseProxy{Transport: transport, FlushInterval: -1,
		Rewrite: func(request *httputil.ProxyRequest) {
			request.SetURL(target)
			request.Out.Host = target.Host
			request.Out.Header.Del("Forwarded")
			request.Out.Header.Del("X-Forwarded-For")
			request.Out.Header.Del("X-Forwarded-Host")
			request.Out.Header.Del("X-Forwarded-Proto")
			request.Out.Header.Set(PeerHeader, request.In.RemoteAddr)
			request.Out.Header.Set(MachineHeader, request.In.Context().Value(machineKey{}).(string))
		},
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, _ error) {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusBadGateway)
			io.WriteString(w, `{"error":"embedded_bridge_unavailable"}`)
		},
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if _, err := PeerAddress(r.RemoteAddr); err != nil {
			http.Error(w, "invalid remote peer", http.StatusForbidden)
			return
		}
		// The companion API and nothing else, in origin form: no absolute-form or
		// asterisk request targets, and no dashboard assets or development pages.
		if !strings.HasPrefix(r.RequestURI, "/api/v1/") || !strings.HasPrefix(r.URL.Path, "/api/v1/") {
			http.NotFound(w, r)
			return
		}
		// An unknown sender is refused: forwarding without its machine would skip the binding.
		machine, err := machineOf(r.Context(), r.RemoteAddr)
		if err != nil || machine == "" {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusServiceUnavailable)
			io.WriteString(w, `{"error":"embedded_peer_unknown"}`)
			return
		}
		proxy.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), machineKey{}, machine)))
	})
}

// ServePond terminates the same pinned HTTPS identity on the embedded tailnet.
// The handler sees the real tailnet peer, never the forwarding socket's loopback.
func ServePond(n *Node, port int, socket, identity string) (*http.Server, <-chan error, error) {
	if port < 1 || port > 65535 {
		return nil, nil, errors.New("invalid companion port")
	}
	if _, err := LoadCertificate(identity); err != nil {
		return nil, nil, err
	}
	listener, err := n.Server.Listen("tcp", ":"+strconv.Itoa(port))
	if err != nil {
		return nil, nil, err
	}
	config := &tls.Config{MinVersion: tls.VersionTLS12, NextProtos: []string{"http/1.1"},
		GetCertificate: func(*tls.ClientHelloInfo) (*tls.Certificate, error) { return LoadCertificate(identity) }}
	server := &http.Server{Handler: BridgeHandler(socket, n.MachineOf), ReadHeaderTimeout: 5 * time.Second, IdleTimeout: 60 * time.Second, MaxHeaderBytes: 32768}
	finished := make(chan error, 1)
	go func() {
		finished <- server.Serve(tls.NewListener(netutil.LimitListener(listener, MaxConnections), config))
	}()
	return server, finished, nil
}
