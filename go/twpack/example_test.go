// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

package twpack_test

import (
	"context"
	"fmt"

	"github.com/seraphinastudios/tilewright-world/go/twpack"
)

// Opens and verifies a pack, then reads every chunk of every plane.
func Example() {
	ctx := context.Background()
	if err := load(ctx, "dist/pack.twpack"); err != nil {
		fmt.Println(twpack.CodeOf(err), err)
	}
}

func load(ctx context.Context, path string) error {
	pack, err := twpack.Open(ctx, path, twpack.Limits{})
	if err != nil {
		return err
	}
	defer pack.Close()
	if _, err := pack.Verify(ctx, twpack.Limits{}); err != nil {
		return err
	}
	profile, err := pack.Profile(ctx)
	if err != nil {
		return err
	}
	for plane, err := range pack.Planes(ctx) {
		if err != nil {
			return err
		}
		for layer, err := range pack.Layers(ctx, plane.ID) {
			if err != nil {
				return err
			}
			for key, err := range pack.ChunkKeys(ctx, plane.ID, layer) {
				if err != nil {
					return err
				}
				chunk, _, err := pack.Chunk(ctx, plane.ID, layer, key.CX, key.CY)
				if err != nil {
					return err
				}
				fmt.Println(plane.ID, layer, key.CX, key.CY, len(chunk.Ground), "of",
					len(profile.GroundLayers), "ground layers,", len(chunk.Objects), "objects")
			}
		}
	}
	return nil
}
