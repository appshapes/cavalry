package main

import (
	"encoding/binary"
	"fmt"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

func main() {
	client, err := cavalry.NewClient("") // the driver at CAVALRY_DIR, or its default
	if err != nil {
		panic(err)
	}
	defer client.Close()
	subscription, err := client.AddSubscription("cavalry:ipc", 1001)
	if err != nil {
		panic(err)
	}
	publication, err := client.AddPublication("cavalry:ipc", 1001)
	if err != nil {
		panic(err)
	}
	received := make([]uint32, 0, 1000)
	onFragment := func(body []byte, header *cavalry.Header) { // body: a slice into the log, for the call
		received = append(received, binary.LittleEndian.Uint32(body))
	}
	for sent := uint32(0); sent < 1000; {
		if publication.TryClaim(32) < 0 { // the position after the frame, or a code below 0
			subscription.Poll(onFragment, 10) // back-pressured or not yet connected: drain, retry
			continue
		}
		binary.LittleEndian.PutUint32(publication.Claim, sent) // write into the log
		publication.Commit()
		sent++
	}
	for len(received) < 1000 {
		subscription.Poll(onFragment, 10)
	}
	for i, value := range received {
		if value != uint32(i) {
			panic("every message once, in order")
		}
	}
	fmt.Printf("%d messages over cavalry:ipc, position %d\n", len(received), publication.Position())
}
