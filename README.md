# CS168 IP Project

This is a project for IP.

## Design

We plan on dividing our implementation into the following layers layers, ordered from top to down:

- Protocol
- Routing
- Network

Below we describe the responsibilities of each layer, as well as the interfaces between each layer.

### Network layer

The network layer is responsible for delivering bytes from one host to another. Given some bytes, it sends the bytes using an interface on a router to an interface of the destination router. It also provides a way to listen for packets that are sent to this host.

#### APIs

```
/// Send data out using a link.
fn send(bytes: &[u8], link_no: u16);

/// Listen for data sent to this host.
fn listen() -> Receiver<Vec<u8>>;
```

#### Implementation

Its implementation uses UDP sockets to establish links between interfaces from two hosts.

### Routing layer

The routing layer is responsible for determining where a packet should go next. A packet can be routed in the following ways:

1. to the host itself ("upstream")
2. to some other destination (send to next-hop)
3. dropped

If a packet is for the host itself, the routing layer inspects the packet's protocol, and calls into one of the registered handlers. The protocol layer decides how to further handle this packet.

If a packet should be sent to some other destination, the router inspects the forwarding table to determine its next hop. Next, it passes this packet to the appropriate link using the `send()` API from the network layer.

If a packet is dropped, the routing layer simply moves on to the next packet.

#### APIs

```
/// Provide a handler function / struct for a protocol
fn register_handler(protocol: u16, handler: Handler);

/// Update routing table
fn update_routing_table(addr: u32, cost: u32);
```

#### Implementation

The forwarding table can be implemented as a linked-list of table entries. Using a linked-list makes insertion and removal easy, and fits with the sequential scan access pattern.

The router can also spawn two threads to perform periodic tasks:

- Table pruning: one thread can wake up periodically to delete entries that have not been updated for 12 seconds.
- Periodic updates: one thread can wake up every 5 seconds to send its routing table state to all of its interfaces.

When a table entry is upserted (via `update_routing_table`), the router also performs triggered update, sending the update to all of its interfaces.

### Protocol layer

The protocol layer specifies how to handle packets. For IP, we'll implement two protocols:

- Test Protocol
- RIP Protocol

The test protocol handler simply prints a packet's payload to stdout.

The RIP protocol handler is responsible for updating the routing table. For each entry in a RIP packet, the handler performs the updates to the forwarding table.
