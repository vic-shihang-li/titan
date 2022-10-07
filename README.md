# CS168 IP Project

This is a project for IP. 

## Design Components
1. Router
   1. Routing Tables
      1. Get/Set
   2. Message Passing
   3. REPL
2. Link Layer Abstraction
   



### Link Layer
Some threads for sending messages to ports, some threads for reading messages from socket.

#### Objects
Link_Table: Vector of Type (interface_name, (state, port))

#### Interfaces

send_packet(packet, destination) {
    // send packet to destination via UDP socket
}

receive_packet() {
    // receive packet from UDP socket
}


### Router


#### Objects
Routing_Table: Vector of Type (destination, (next_hop, cost, last_update))
Interface_Lookup: Vector of Type (next_hop, interface_name)

#### Interfaces

update_route_table(interface_name, state, old_port, new_port) {
    // update link table based on RIP Handler
}

link_table_cleanup() {
    // remove old entries from link table based on timestamp
}

RIPHandler(routing_table, link_table) {
    // updates routing_table, link_table based on heartbeat
    // sends RIP messages to neighbors
}

REPL() {
    // REPL for router
}

### Top Layer
Parsing/Printing packets to/from console.
### Middle Layer
Checking for destination, forwarding, etc. 
Layer 1: RIP Handler, Test_Message Handler
Layer 2: Routing Handler (handles whether to forward or consume packet).
### Bottom Layer
Sending/Receiving packets from UDP socket