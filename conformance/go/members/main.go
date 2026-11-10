// Prints a members string for a cluster of three members on the loopback, each with five
// free UDP ports, `id,ingress,consensus,log,catchup,archive` joined by `|`
// (docs/how-to/run-a-cluster.md), for conformance/go/run.sh to start the members with. The
// fifteen sockets are held until every port is chosen, so that none repeats. Go's standard
// library only (ADR 0003).
package main

import (
	"fmt"
	"net"
	"os"
	"strings"
)

func main() {
	var sockets []*net.UDPConn
	var ports []int
	for i := 0; i < 15; i++ {
		socket, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
		if err != nil {
			fmt.Fprintf(os.Stderr, "conformance/go: members: no free loopback UDP port: %v\n", err)
			os.Exit(1)
		}
		sockets = append(sockets, socket)
		ports = append(ports, socket.LocalAddr().(*net.UDPAddr).Port)
	}
	for _, socket := range sockets {
		socket.Close()
	}
	var members []string
	for member := 0; member < 3; member++ {
		var endpoints []string
		for _, port := range ports[member*5 : member*5+5] {
			endpoints = append(endpoints, fmt.Sprintf("127.0.0.1:%d", port))
		}
		members = append(members, fmt.Sprintf("%d,%s", member, strings.Join(endpoints, ",")))
	}
	fmt.Println(strings.Join(members, "|"))
}
