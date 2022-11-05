struct VTcpListener {
    port: u16,
}

struct VTcpConn {}

pub async fn v_listen(port: u16) -> Result<VTcpListener> {
    VTcpListener{port: port}
}
