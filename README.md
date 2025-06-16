# IM Backend Architecture Description

## Project Overview

This project provides an implementation of a backend for an Instant Messaging (IM) system, developed in the Rust programming language, with a microservice architecture design. Its primary goal is to offer developers an efficient, stable, and scalable IM solution. The focus of this project is to explore and demonstrate the potential and practices of Rust in building high-performance backend services.

## Key Features

- **Microservice Architecture:** The system is split into multiple independent service units, each responsible for a portion of the core business logic and communication with other services.
- **Containerized Deployment:** All services can be packaged with Docker, facilitating deployment and management.
- **Asynchronous Processing:** Utilizes Rust's asynchronous programming capabilities to handle concurrent workloads, enhancing performance and throughput.
- **Data Storage:** Uses PostgreSQL and MongoDB for storing messages permanently and for inbox functionalities, respectively.
- **Message Queue:** Leverages Kafka as a message queue to support high concurrency message pushing and processing.
- **Hybrid Sharded Storage:** Implements time-based and user ID-based hybrid sharding for MongoDB message storage, improving performance and scalability.

## Architecture Components

![architecture](rfcs/images/framework-english.png)

1. **Service Layer**

   - **Authentication Service:** Handles user registration, login, and verification.
   - **Message Service:** Responsible for message sending, receiving, and forwarding.
   - **Friend Service:** Manages the user's friends list and status.
   - **Group Service:** Takes care of group creation, message broadcasting, and member management.

2. **Data Storage Layer**

   - **PostgreSQL:** Storing user information, friendship relations, and message history, along with automated archival through scheduled tasks.
   - **MongoDB:** Acts as a message inbox, handling offline message storage and retrieval with hybrid sharding strategy.

3. **Middleware Layer**

   - **Kafka:** Provides a high-throughput message queue to decouple services.
   - **Redis:** Implements caching and maintains message status to optimize database load.

4. **Infrastructure Layer**

   - **Docker and Docker-Compose:** Containers for building and deploying services.
   - **Synapse:** For service registration and discovery.
   - **MinIO:** An object storage solution for handling file uploads and downloads.

## Performance and Scalability

   The project is designed with high performance and horizontal scalability in mind. Through asynchronous processing and a microservice architecture, the system is capable of scaling effectively by increasing the number of service instances in response to the growing load. Additionally, the project adopts a modular design philosophy that allows developers to customize or replace modules as needed.

   The MongoDB message storage uses a hybrid sharding strategy that combines time-based and user ID-based sharding to improve performance significantly:

- **Time-based Sharding:** Messages are partitioned by month for efficient archival and time-range queries
- **User ID-based Sharding:** Messages are further distributed by user ID hash to prevent hotspots
- This hybrid approach has shown 30-50% throughput improvement in high-load scenarios

## Unresolved questions and Future Enhancements

- **Conversation Feature:** There is currently no implementation of conversations on the server-side, as it exists only client-side.
- **User Login Device Field:** Adding a field for tracking login devices in the user table, to check if clients need to sync the friend list.
- **Friendship Read Status:** Implementing deletion of friendship-related messages after user reads them.
- **Multi-device Management:** Handling friendship/group operations when users are online from multiple devices simultaneously.
- **Device Management:** Implementation of functionality to log out remote desktop sessions from mobile devices.
- **File Cleanup:** Adding periodic deletion of MinIO files.
- **Matrix Protocol Support:** Adding support for Matrix protocol to enable robot integration.
- **Redis Optimization:** Combining query send_seq and incr recv_seq into one operation with Lua scripts.
- **Error Handling Enhancement:** Adding detailed error information for logs while keeping frontend responses concise.

## Development

1. Install `librdkafka`

   **Ubuntu：**

   ```shell
   apt install librdkafka-dev
   ```

   **Windows:**

   ```shell
   # install vcpkg
   git clone https://github.com/microsoft/vcpkg
   cd vcpkg
   .\bootstrap-vcpkg.bat
   # Install librdkafka
   vcpkg install librdkafka
   .\vcpkg integrate install
   ```

2. Run docker compose

   ```shell
   docker-compose up -d
   ```

   **Important:** Make sure all third-party services are running in Docker.

3. Install sqlx-cli and initialize the database

   ```shell
   cargo install sqlx-cli
   sqlx migrate run
   ```

4. Clone the project

   ```shell
   git clone https://github.com/Xu-Mj/sandcat-backend.git
   cd sandcat-backend
   ```

5. Build

   - **Linux：** Use the static feature

     ```shell
     cargo build --release --features=static --no-default-features
     ```

   - **Windows:** Use the dynamic feature

     ```shell
     cargo build --release --features=dynamic
     ```

6. Copy the binary file to root path

   ```shell
   cp target/release/cmd ./sandcat
   ```

7. Run

   ```shell
   ./sandcat
   ```

   If you need to adjust configurations, please modify `config.yml`. For MongoDB sharding configuration, ensure the following settings are in your config:

   ```yaml
   db:
     mongodb:
       # ... other MongoDB settings ...
       use_sharding: true
       user_shards: 10  # Adjust based on your needs
   ```

**Important:** Given that our working environments may differ, should you encounter any errors during your deployment, please do let me know. Together, we'll work towards finding a solution.

## Contributing

We follow [Trunk's](https://github.com/trunk-rs/trunk.git) Contribution Guidelines. They are doing a great job.

Anyone and everyone is welcome to contribute! Please review the [CONTRIBUTING.md](./CONTRIBUTING.md) document for more details. The best way to get started is to find an open issue, and then start hacking on implementing it. Letting other folks know that you are working on it, and sharing progress is a great approach. Open pull requests early and often, and please use GitHub's draft pull request feature.

## License

sandcat is licensed under the terms of the MIT License.
