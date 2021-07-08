use std::future::Future;

use anyhow::Result;
use wasmtime::{Caller, Linker, Trap};

use crate::{
    api::{error::IntoTrap, get_memory},
    message::Message,
    state::State,
};

use super::{link_async1_if_match, link_if_match};

// Register the mailbox APIs to the linker
pub(crate) fn register(linker: &mut Linker<State>, namespace_filter: &Vec<String>) -> Result<()> {
    link_if_match(
        linker,
        "lunatic::message",
        "create",
        create,
        namespace_filter,
    )?;
    link_if_match(
        linker,
        "lunatic::message",
        "set_buffer",
        set_buffer,
        namespace_filter,
    )?;
    link_if_match(
        linker,
        "lunatic::message",
        "add_process",
        add_process,
        namespace_filter,
    )?;
    link_if_match(
        linker,
        "lunatic::message",
        "add_tcp_stream",
        add_tcp_stream,
        namespace_filter,
    )?;
    link_if_match(linker, "lunatic::message", "send", send, namespace_filter)?;
    link_async1_if_match(
        linker,
        "lunatic::message",
        "prepare_receive",
        prepare_receive,
        namespace_filter,
    )?;
    link_if_match(
        linker,
        "lunatic::message",
        "receive",
        receive,
        namespace_filter,
    )?;
    Ok(())
}

//% lunatic::message::create()
//%
//% Creates a new message. This message is intended to be modified by other functions in this
//% namespace. Once `lunatic::message::send` is called it will be sent to another process.
fn create(mut caller: Caller<State>) {
    caller.data_mut().message = Some(Message::default());
}

//% lunatic::message::set_buffer(
//%     data_ptr: i32,
//%     data_len: i32,
//% )
//%
//% Sets the data for the next message.
//%
//% Traps:
//% * If **data_ptr + data_len** is outside the memory.
//% * If it's called before the next message is created.
fn set_buffer(mut caller: Caller<State>, data_ptr: u32, data_len: u32) -> Result<(), Trap> {
    let mut buffer = vec![0; data_len as usize];
    let memory = get_memory(&mut caller)?;
    memory
        .read(&caller, data_ptr as usize, buffer.as_mut_slice())
        .or_trap("lunatic::message::set_buffer")?;
    caller
        .data_mut()
        .message
        .as_mut()
        .or_trap("lunatic::message::set_buffer")?
        .set_buffer(buffer);
    Ok(())
}

//% lunatic::message::add_process(process_id: i64) -> i32
//%
//% Adds a process resource to the next message and returns the location in the array the process
//% was added to. This will remove the process handle from the current process' resources.
//%
//% Traps:
//% * If process ID doesn't exist
//% * If it's called before the next message is created.
fn add_process(mut caller: Caller<State>, process_id: u64) -> Result<u32, Trap> {
    let process = caller
        .data_mut()
        .resources
        .processes
        .remove(process_id)
        .or_trap("lunatic::message::add_process")?;
    Ok(caller
        .data_mut()
        .message
        .as_mut()
        .or_trap("lunatic::message::add_process")?
        .add_process(process) as u32)
}

//% lunatic::message::add_tcp_stream(stream_id: i64) -> i32
//%
//% Adds a TCP stream resource to the next message and returns the location in the array the TCP
//% stream was added to. This will remove the TCP stream from the current process' resources.
//%
//% Traps:
//% * If TCP stream ID doesn't exist
//% * If it's called before the next message is created.
fn add_tcp_stream(mut caller: Caller<State>, stream_id: u64) -> Result<u32, Trap> {
    let stream = caller
        .data_mut()
        .resources
        .tcp_streams
        .remove(stream_id)
        .or_trap("lunatic::message::add_tcp_stream")?;
    Ok(caller
        .data_mut()
        .message
        .as_mut()
        .or_trap("lunatic::message::add_tcp_stream")?
        .add_tcp_stream(stream) as u32)
}

//% lunatic::message::send(
//%     process_id: i64,
//% ) -> i32
//%
//% Returns:
//% * 0 on success
//% * 1 on error   - Process can't receive messages (finished).
//%
//% Sends the message to a process.
//%
//% Traps:
//% * If the process ID doesn't exist.
//% * If it's called before a creating the next message.
fn send(
    mut caller: Caller<State>,
    process_id: u64,
    data_ptr: u32,
    data_len: u32,
) -> Result<u32, Trap> {
    let mut buffer = vec![0; data_len as usize];
    let memory = get_memory(&mut caller)?;
    memory
        .read(&caller, data_ptr as usize, buffer.as_mut_slice())
        .or_trap("lunatic::message::send")?;
    let message = caller
        .data_mut()
        .message
        .take()
        .or_trap("lunatic::message::send")?;
    let process = caller
        .data()
        .resources
        .processes
        .get(process_id)
        .or_trap("lunatic::message::send")?;
    let result = match process.send_message(message) {
        Ok(()) => 0,
        Err(_error) => 1,
    };
    Ok(result)
}

//% lunatic::message::prepare_receive(i32_msg_size_ptr: i32, i32_res_size_ptr: i32) -> i32
//%
//% Returns:
//% * 0 on success - The size of the message buffer is written to **i32_msg_size_ptr** and the
//%                  number of the resources is written to **i32_res_size_ptr**.
//% * 1 on error   - Process can't receive more messages (nobody holds a handle to it).
//%
//% This function should be called before `lunatic::message::receive` to let the guest know how
//% much memory space needs to be reserved for the next message.
//%
//% Traps:
//% * If **size_ptr** is outside the memory.
fn prepare_receive(
    mut caller: Caller<State>,
    size_ptr: u32,
) -> Box<dyn Future<Output = Result<i32, Trap>> + Send + '_> {
    Box::new(async move {
        let message = match caller.data_mut().mailbox.recv().await {
            Some(message) => message,
            None => return Ok(1),
        };

        let message_buffer_size = message.buffer_size() as u32;
        caller.data_mut().message = Some(message);
        let memory = get_memory(&mut caller)?;
        memory
            .write(
                &mut caller,
                size_ptr as usize,
                &message_buffer_size.to_le_bytes(),
            )
            .or_trap("lunatic::message::prepare_receive")?;
        Ok(0)
    })
}

//% lunatic::message::receive(data_ptr: i32, resource_ptr: i32)
//%
//% * **data_ptr**     - Pointer to write the data to.
//% * **resource_ptr** - Pointer to an array of i64 values, where each value represents the
//%                      resource id inside the new process. Resources are in the same order they
//%                      were added.
//%
//% Writes the message that was prepared with `lunatic::message::prepare_receive` to the guest.
//%
//% Traps:
//% * If `lunatic::message::prepare_receive` was not called before.
//% * If **data_ptr + size of the message** is outside the memory.
//% * If **resource_ptr + size of the resources** is outside the memory.
fn receive(mut caller: Caller<State>, data_ptr: u32, resource_ptr: u32) -> Result<(), Trap> {
    let last_message = caller
        .data_mut()
        .message
        .take()
        .or_trap("lunatic::message::receive")?;
    let memory = get_memory(&mut caller)?;
    memory
        .write(&mut caller, data_ptr as usize, last_message.buffer())
        .or_trap("lunatic::message::receive")?;

    let resources: Vec<u8> = last_message
        .resources()
        .into_iter()
        .map(|resource| match resource {
            crate::message::Resource::Process(process_handle) => {
                u64::to_le_bytes(caller.data_mut().resources.processes.add(process_handle))
            }
            crate::message::Resource::TcpStream(tcp_stream) => {
                u64::to_le_bytes(caller.data_mut().resources.tcp_streams.add(tcp_stream))
            }
        })
        .flatten()
        .collect();
    memory
        .write(&mut caller, resource_ptr as usize, &resources)
        .or_trap("lunatic::message::receive")?;
    Ok(())
}
