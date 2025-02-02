mod common;
use common::{array, spawn_server_as_slave, spawn_server_process, wait_for_message};
use duva::client_utils::ClientStreamHandler;

#[tokio::test]
async fn test_cluster_known_nodes_increase_when_new_replica_is_added() {
    // GIVEN
    let mut master_process = spawn_server_process();
    let mut client_handler = ClientStreamHandler::new(master_process.bind_addr()).await;

    let cmd = &array(vec!["cluster", "info"]);

    let mut repl_p = spawn_server_as_slave(&master_process);
    repl_p.wait_for_message(&master_process.heartbeat_msg(0), 1);
    master_process.wait_for_message(&repl_p.heartbeat_msg(0), 1);

    client_handler.send(cmd).await;
    let cluster_info = client_handler.get_response().await;
    assert_eq!(cluster_info, array(vec!["cluster_known_nodes:1"]));

    // WHEN -- new replica is added
    let mut new_repl = spawn_server_as_slave(&master_process);
    new_repl.wait_for_message(&master_process.heartbeat_msg(0), 1);

    //THEN
    client_handler.send(cmd).await;
    let cluster_info = client_handler.get_response().await;
    assert_eq!(cluster_info, array(vec!["cluster_known_nodes:2"]));
}

#[tokio::test]
async fn system_removes_node_when_heartbeat_is_not_received_for_certain_time() {
    // GIVEN
    let mut master_process = spawn_server_process();

    let cmd = &array(vec!["cluster", "info"]);

    let mut replica_process = spawn_server_as_slave(&master_process);
    let mut stdout_for_repl1 = replica_process.stdout.take().unwrap();
    wait_for_message(&mut stdout_for_repl1, "[INFO] from master rh:", 1);
    let mut master_stdout = master_process.stdout.take().unwrap();
    wait_for_message(&mut master_stdout, "[INFO] from replica rh:", 1);
    let mut h = ClientStreamHandler::new(master_process.bind_addr()).await;
    h.send(cmd).await;
    let cluster_info = h.get_response().await;
    assert_eq!(cluster_info, array(vec!["cluster_known_nodes:1"]));

    // WHEN
    replica_process.kill().unwrap();
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    h.send(cmd).await;
    let cluster_info = h.get_response().await;

    //THEN
    assert_eq!(cluster_info, array(vec!["cluster_known_nodes:0"]));
}
