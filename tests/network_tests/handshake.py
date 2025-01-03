#!/usr/bin/env python3
import os
import sys

sys.path.insert(1, os.path.dirname(sys.path[0]))

from test_framework.test_framework import ConfluxTestFramework
from test_framework.mininode import DefaultNode
from test_framework.util import wait_until, connect_nodes

class HandshakeTests(ConfluxTestFramework):
    def set_test_params(self):
        self.num_nodes = 2

    def setup_network(self):
        self.setup_nodes()

    def run_test(self):
        genesis = self.nodes[0].cfx_getBlockByEpochNumber("0x0", False)["hash"]
        # mininode handshake
        peer = DefaultNode(genesis)
        self.nodes[0].add_p2p_connection(peer)
        wait_until(lambda: peer.had_status, timeout=3)

        # full node handshake
        connect_nodes(self.nodes, 0, 1, timeout=3)

if __name__ == "__main__":
    HandshakeTests().main()